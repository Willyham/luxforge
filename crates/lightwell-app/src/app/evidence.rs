//! Evidence mode: import queued files in order, capture a frame after each outcome, run any script
//! steps with a frame each, then exit. Every step goes through the same messages and owner calls the
//! controls use, so a script proves the real paths rather than a parallel implementation.
use crate::{
    app::{
        Editor,
        fields::number_text,
        message::{
            BrushEdit, CropMessage, CropPointer, MaskMessage, MenuTarget, Message, PaintTarget,
            PaletteAction, PresetMessage, RowEdit,
        },
        tasks::{HostAnswer, host_task, mutation, workspace_task},
    },
    crop_draft::{Corner, Handle},
    mask_draft::MaskDraft,
    state::{
        presets::{PresetRow, presettable_groups},
        tools::crop_frame,
    },
};
use iced::Task;
use lightwell_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Map, Value, json};
use std::{collections::VecDeque, path::PathBuf, time::Duration};

/// An evidence run that has not finished by then is stuck; exit so the harness reaps nothing.
pub(crate) const EVIDENCE_DEADLINE: Duration = Duration::from_secs(25);
/// Full RAW edit/history scripts can redevelop a 100 MP source several times.
/// The Q2 correction journey makes progress beyond the single-open deadline.
pub(crate) const SCRIPT_EVIDENCE_DEADLINE: Duration = Duration::from_secs(60);

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
    /// A paced slider step's values still to send, one per tick of its own gated timer. `None` when
    /// no paced step is running, which is also when the timer that drives it does not exist.
    pub(crate) paced_slider: Option<PacedSlider>,
    /// The gallery page shown instead of the workspace for a scripted capture.
    /// Requested tools-panel scroll fraction, retained beside the capture for correlation.
    pub(crate) tools_scroll: Option<f64>,
}

/// The state of a slider step sent by a timer rather than all at once. Each tick sends the next
/// value through the same messages [`Editor::slider_step`] sends synchronously, then advances or,
/// on the last value, ends the gesture the way the step said to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PacedSlider {
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// Values still to send, in order; the front is sent by the next tick.
    pub(crate) remaining: VecDeque<f64>,
    /// How many of the step's values have already been sent, which is the index the next one
    /// records.
    pub(crate) sent: usize,
    pub(crate) interval_ms: u64,
    pub(crate) end: SliderEnd,
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
    /// A first-slice generated slider (fraction) or discrete control gesture.
    Controls(ControlsStep),
    Picker(PickerStep),
    Curve(CurveStep),
    Group(GroupStep),
    /// The tab a tabbed section shows, as its tab row selects it.
    Tab(TabStep),
    Section(SectionStep),
    Gallery(Option<usize>),
    ToolsScroll(f64),
    /// What one generated field is typed into, and whether Enter is pressed in it.
    Field(FieldStep),
    /// A module or group reset, through the control that declares it.
    Reset(ResetStep),
    /// One click on the photograph at a pixel of the raster on screen, answered by the canvas mode
    /// that is active.
    Pick(PickStep),
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
    /// Click one library preset's row: [`Message::RunAction`] with the section's own action.
    Preset(PresetPick),
    /// Open the create form, type its name and group, set every checkbox and press Create.
    PresetCreate(PresetCreateStep),
    /// Delete one library preset through its row's context menu.
    PresetDelete(PresetPick),
    /// Import one file through the section's own import task, bypassing only the native dialog.
    /// The path is as the script wrote it, relative to the editor's working directory.
    PresetImport(String),
    /// One Masks-panel view or mask-canvas gesture, through the same [`MaskMessage`] the panel's
    /// rows, buttons and the canvas raise.
    Mask(MaskStep),
}

/// How a script names a mask or a component.
///
/// `mask.create-<kind>` assigns the identity, so a script that creates a mask in one step has no
/// identity to write into the next one. The display name the host gave it — `Mask 1`, `Linear 1` —
/// is what a script written before the run can put there instead, resolved against the `mask.list`
/// answer the editor is holding when the step runs; a name that matches nothing, or more than one
/// row, fails the step rather than guessing. A position in the list is the third spelling, for the
/// steps that drive the panel as a pointer does, where the row and not its name is what the gesture
/// means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reference {
    /// The identity itself, written as a plain string.
    Id(String),
    /// The name the host gave it, written as `{"name": "…"}`.
    Name(String),
    /// A position in the list the panel shows, written as a bare integer.
    Index(usize),
}

impl Reference {
    fn record(&self) -> Value {
        match self {
            Self::Id(id) => json!(id),
            Self::Name(name) => json!({ "name": name }),
            Self::Index(index) => json!(index),
        }
    }

    /// A reference as it may be written anywhere a script names a mask or a component: the identity
    /// as a plain string, `{"name": "…"}`, or a position in the list.
    fn parse(value: &Value, field: &str) -> Result<Self, String> {
        match value {
            Value::String(id) if !id.trim().is_empty() => Ok(Self::Id(id.clone())),
            Value::Number(index) => index
                .as_u64()
                .map(|index| Self::Index(index as usize))
                .ok_or_else(|| format!("{field} takes a position in the list, not {index}")),
            Value::Object(object) => {
                let (key, value) = sole(object)?;
                match (key, value.as_str()) {
                    ("name", Some(name)) if !name.trim().is_empty() => {
                        Ok(Self::Name(name.to_owned()))
                    }
                    ("name", _) => Err(format!("{field} name takes a non-empty string")),
                    (other, _) => Err(format!(
                        "unknown {field} reference field {other}; expected name"
                    )),
                }
            }
            _ => Err(format!(
                "{field} takes an identity, {{\"name\": \"…\"}} or a position in the list"
            )),
        }
    }
}

/// A reference where naming nothing is the other choice: `null` clears a selection, or takes the
/// pointer off the list.
fn reference_or_null(reference: &Option<Reference>) -> Value {
    reference.as_ref().map_or(Value::Null, Reference::record)
}

/// What one `mask` step does to the Masks panel or to the shape on the canvas.
///
/// Each verb is one of the panel's own gestures, so a script drives it through exactly the messages
/// a pointer sends and the requests that reach the owner are the panel's own. The step kind spells
/// no method name of its own: a gesture commits through the draft lifecycle and a row edit through
/// the panel's one row-command builder, which is what the Copy as JSON request beside it reads too.
///
/// Geometry travels in normalized content coordinates, which is exactly what the canvas publishes
/// after mapping the pointer through `render.transform`'s affine.
///
/// One verb per step, and one captured frame per step, so a frame is evidence of one thing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaskStep {
    /// Open one mask, as clicking its row does.
    Select(Reference),
    /// Select one component of the open mask, which shows its handles and its number fields, or
    /// clear the selection.
    SelectComponent(Option<Reference>),
    /// Put the pointer on that component's row, or take it off the list. While a row is hovered the
    /// overlay shows that component's own contribution instead of the composed mask.
    Hover(Option<Reference>),
    /// Reopen one component's geometry as a canvas gesture, so its handles are drawn.
    EditShape(Reference),
    /// The mode the next Add gesture will use, chosen before the gesture as the Add row does.
    Mode(String),
    /// Begin a gesture that creates a mask whose first component is of this kind.
    New(String),
    /// Begin a gesture that adds a component of this kind to the open mask, in the chosen mode.
    Add(String),
    /// Arm the brush: on nothing, which paints a new mask; on the open mask, which puts a further
    /// brush on it in the chosen mode; or on one component, which appends to that brush. The Add row
    /// offers no brush, so this is the route a script takes, exactly as the panel's own Brush
    /// section does.
    Paint(PaintStep),
    /// One change to the brush the next stroke will be drawn with.
    Brush(BrushStep),
    /// Paint one stroke into the open gesture: a press, a move per position and, unless the step
    /// says to leave it down, a release that commits it as **one** history entry. That is the one
    /// place a mask gesture differs from the rest: a person painting does not apply each stroke, so
    /// the release is the commit and the brush stays in hand for the next one.
    Stroke {
        points: Vec<[f64; 2]>,
        release: bool,
    },
    /// A whole shape drawn in one stroke: the press at `from`, the pointer at `to`. The pointer is
    /// still down afterwards, exactly as it is mid-drag, so the release is a step of its own.
    Sweep { from: [f64; 2], to: [f64; 2] },
    /// The pointer lifted. The shape it drew stays; committing it is a separate decision.
    Release,
    /// A press on one drawn handle, the points it is dragged through, and its release.
    Drag {
        handle: String,
        points: Vec<[f64; 2]>,
    },
    /// Commit the open gesture: one history entry.
    Apply,
    /// Discard the open gesture.
    Cancel,
    /// One component row's own list edit, on the row it names rather than on whichever component
    /// happens to be selected.
    Row { component: Reference, edit: RowStep },
    /// Enter or leave the host's own canvas pick for the selected component's kind, which is what
    /// the panel's Pick button does: one `workspace.set`, committing nothing.
    Pick,
}

/// What one component row's control does, before its row is resolved to an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowStep {
    Mode(String),
    Invert(bool),
    Move(usize),
    Delete,
    /// Remove one of this component's strokes, named the same three ways every other object in a
    /// script is: its content address, the label the row shows — `Stroke 1` — or its position in
    /// the list. A forward edit: one entry appended, every entry after the stroke left where it is.
    DeleteStroke(Reference),
}

/// What the next stroke will land on, named before the gesture rather than guessed from where the
/// pointer went down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaintStep {
    /// A new mask whose first component is an add brush.
    NewMask,
    /// A further brush on the open mask, in the mode the Add row has chosen.
    NewBrush,
    /// Another stroke on that component of the open mask.
    Component(Reference),
}

/// One change to the brush, each field named exactly as `mask.add-stroke` declares it. A script sets
/// what it means to set rather than counting key presses, and the held modifier is its own field
/// because it is a hold and not a latch.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BrushStep {
    pub(crate) size: Option<f64>,
    pub(crate) feather: Option<f64>,
    pub(crate) flow: Option<f64>,
    pub(crate) erase: Option<bool>,
    pub(crate) erase_held: Option<bool>,
    /// Hold the next stroke to the colour under the brush where it begins. The script sets the flag
    /// and never a colour: the host reads the pixel the masked operation receives at the stroke's
    /// first position, exactly as it does for a pointer.
    pub(crate) limit_to_colour: Option<bool>,
    pub(crate) colour_refine: Option<f64>,
    /// Move one declared field by that many of its own declared steps, which is what a bracket key
    /// does. Named so a script can prove the key and the panel move by the same amount.
    pub(crate) nudge: Option<(String, f64)>,
}

impl PaintStep {
    fn record(&self) -> Value {
        match self {
            Self::NewMask => json!("new-mask"),
            Self::NewBrush => json!("new-brush"),
            Self::Component(component) => json!({ "component": component.record() }),
        }
    }
}

impl BrushStep {
    fn record(&self) -> Value {
        let mut value = Map::new();
        for (name, number) in [
            ("size", self.size),
            ("feather", self.feather),
            ("flow", self.flow),
            ("colour_refine", self.colour_refine),
        ] {
            if let Some(number) = number {
                value.insert(name.into(), json!(number));
            }
        }
        for (name, flag) in [
            ("erase", self.erase),
            ("erase_held", self.erase_held),
            ("limit_to_colour", self.limit_to_colour),
        ] {
            if let Some(flag) = flag {
                value.insert(name.into(), json!(flag));
            }
        }
        if let Some((name, steps)) = &self.nudge {
            value.insert("nudge".into(), json!([name, steps]));
        }
        Value::Object(value)
    }
}

impl MaskStep {
    fn record(&self) -> Value {
        match self {
            Self::Select(mask) => json!({ "select": mask.record() }),
            Self::SelectComponent(component) => {
                json!({ "select_component": reference_or_null(component) })
            }
            Self::Hover(component) => json!({ "hover": reference_or_null(component) }),
            Self::EditShape(component) => json!({"edit_shape": component.record()}),
            Self::Mode(mode) => json!({ "mode": mode }),
            Self::New(kind) => json!({ "new": kind }),
            Self::Add(kind) => json!({ "add": kind }),
            Self::Paint(target) => json!({ "paint": target.record() }),
            Self::Brush(step) => json!({ "brush": step.record() }),
            Self::Stroke { points, release } => {
                json!({"stroke":{"points":points,"release":release}})
            }
            Self::Sweep { from, to } => json!({"sweep":{"from":from,"to":to}}),
            Self::Release => json!({ "release": true }),
            Self::Drag { handle, points } => json!({"drag":{"handle":handle,"points":points}}),
            Self::Apply => json!({ "apply": true }),
            Self::Cancel => json!({ "cancel": true }),
            Self::Pick => json!({ "pick": true }),
            Self::Row { component, edit } => json!({"row": match edit {
                RowStep::Mode(mode) => json!({"component":component.record(),"mode":mode}),
                RowStep::Invert(invert) => json!({"component":component.record(),"invert":invert}),
                RowStep::Move(index) => json!({"component":component.record(),"index":index}),
                RowStep::Delete => json!({"component":component.record(),"delete":true}),
                RowStep::DeleteStroke(stroke) => {
                    json!({"component":component.record(),"delete_stroke":stroke.record()})
                }
            }}),
        }
    }
}

/// One library preset, by its exact name, and by its group when two groups hold that name. A step
/// that matches no row, or more than one, fails rather than guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetPick {
    pub(crate) name: String,
    pub(crate) group: Option<String>,
}

/// The create form as a script fills it: the name, the group when it is not the default, and the
/// labels of exactly the checkboxes to leave checked. Without `submit` the form is left open and
/// filled, so its frame shows the form itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetCreateStep {
    pub(crate) name: String,
    pub(crate) group: Option<String>,
    pub(crate) groups: Vec<String>,
    pub(crate) submit: bool,
}

impl PresetPick {
    fn record(&self) -> Value {
        match &self.group {
            Some(group) => json!({"name":self.name,"group":group}),
            None => json!({"name":self.name}),
        }
    }
}

/// One slider gesture. Every value becomes one `SliderMoved` with a tick between them, exactly as
/// a pointer drag and the gated subscription produce them; the gesture then ends the way `end`
/// says, or stays open when it says nothing.
///
/// Without `interval_ms` every value is sent at once, as a fast drag would coalesce between ticks.
/// With it, one value is sent per tick of its own gated timer instead, which is how a wild,
/// undrained drag is scripted: `interval_ms` paces the values in real time so the driver's own
/// coalescing runs on them, rather than the harness deciding what reaches the owner.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderStep {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) values: Vec<f64>,
    pub(crate) end: SliderEnd,
    pub(crate) interval_ms: Option<u64>,
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
pub(crate) enum ControlsStep {
    Slider {
        action: String,
        parameter: String,
        fractions: Vec<f64>,
        finish: SliderEnd,
    },
    Discrete {
        action: String,
        parameter: String,
        value: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PickerStep {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) open: Option<bool>,
    pub(crate) hue: Option<f32>,
    pub(crate) plane: Option<[f32; 2]>,
    pub(crate) finish: SliderEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CurveStep {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) event: CurveStepEvent,
    pub(crate) finish: SliderEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CurveStepEvent {
    Move { index: usize, points: Vec<[f32; 2]> },
    Add([f32; 2]),
    Remove(usize),
    Channel(usize),
}

#[derive(Clone, Copy)]
enum GeneratedKind {
    Slider,
    Picker,
    Curve,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupStep {
    pub(crate) module: String,
    pub(crate) path: Vec<usize>,
    pub(crate) expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabStep {
    pub(crate) module: String,
    pub(crate) index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SectionStep {
    pub(crate) module: String,
    pub(crate) expanded: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldStep {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) text: String,
    /// Enter in the field, which commits that one field without a draft.
    pub(crate) submit: bool,
}

/// One canvas pick, in pixels of the raster on screen: the same coordinates the canvas publishes
/// when a pointer is pressed over the photograph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PickStep {
    pub(crate) x: u32,
    pub(crate) y: u32,
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
    /// What the canvas draws of the selected mask, and in which tint. Per-client view state as the
    /// clipping overlays are, and drivable here for the same reason: a captured frame that shows
    /// the overlay must have been put into that state through the path the desktop itself uses.
    pub(crate) mask_overlay: Option<String>,
    pub(crate) mask_overlay_colour: Option<String>,
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
    /// A drag on the angle's rail through these fractions of its range, then its release, exactly
    /// as the rail publishes them.
    AngleRail(Vec<f64>),
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
            Self::Slider(slider) => {
                let mut object = json!({
                    "action": slider.action,
                    "parameter": slider.parameter,
                    "values": slider.values,
                    "release": slider.end == SliderEnd::Release,
                    "cancel": slider.end == SliderEnd::Cancel,
                });
                if let Some(interval_ms) = slider.interval_ms {
                    object["interval_ms"] = json!(interval_ms);
                }
                json!({"slider": object})
            }
            Self::Controls(ControlsStep::Slider {
                action,
                parameter,
                fractions,
                finish,
            }) => json!({
                "controls":{"action":action,"parameter":parameter,"gesture":"slider",
                    "fractions":fractions,"finish":finish.name()}
            }),
            Self::Controls(ControlsStep::Discrete {
                action,
                parameter,
                value,
            }) => json!({
                "controls":{"action":action,"parameter":parameter,"gesture":"discrete","value":value}
            }),
            Self::Picker(step) => {
                let mut value = json!({"action":step.action,"parameter":step.parameter,
                    "finish":step.finish.name()});
                if let Some(open) = step.open {
                    value["open"] = json!(open);
                }
                if let Some(hue) = step.hue {
                    value["hue"] = json!(hue);
                }
                if let Some(plane) = step.plane {
                    value["plane"] = json!(plane);
                }
                json!({"picker":value})
            }
            Self::Curve(step) => {
                let mut value = json!({"action":step.action,"parameter":step.parameter});
                match &step.event {
                    CurveStepEvent::Move { index, points } => {
                        value["event"] = json!("move");
                        value["index"] = json!(index);
                        value["points"] = json!(points);
                        value["finish"] = json!(step.finish.name());
                    }
                    CurveStepEvent::Add(point) => {
                        value["event"] = json!("add");
                        value["point"] = json!(point);
                    }
                    CurveStepEvent::Remove(index) => {
                        value["event"] = json!("remove");
                        value["index"] = json!(index);
                    }
                    CurveStepEvent::Channel(index) => {
                        value["event"] = json!("channel");
                        value["index"] = json!(index);
                    }
                }
                json!({"curve":value})
            }
            Self::Group(step) => json!({"group":{"module":step.module,"path":step.path,
                "expanded":step.expanded}}),
            Self::Tab(step) => json!({"tab":{"module":step.module,"index":step.index}}),
            Self::Section(step) => json!({"section":{"module":step.module,
                "expanded":step.expanded}}),
            Self::Gallery(page) => json!({"gallery":{"page":page}}),
            Self::ToolsScroll(fraction) => json!({"tools_scroll":fraction}),
            Self::Field(field) => json!({"field":{
                "action": field.action,
                "parameter": field.parameter,
                "text": field.text,
                "submit": field.submit,
            }}),
            Self::Pick(pick) => json!({"pick":{"x":pick.x,"y":pick.y}}),
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
            Self::Preset(pick) => json!({"preset":pick.record()}),
            Self::PresetDelete(pick) => json!({"preset_delete":pick.record()}),
            Self::PresetCreate(step) => {
                let mut value = json!({"name":step.name,"groups":step.groups});
                if let Some(group) = &step.group {
                    value["group"] = json!(group);
                }
                if !step.submit {
                    value["submit"] = json!(false);
                }
                json!({"preset_create":value})
            }
            Self::PresetImport(path) => json!({"preset_import":{"path":path}}),
            Self::Mask(step) => json!({ "mask": step.record() }),
        }
    }
}

impl SliderEnd {
    fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Release => "release",
            Self::Cancel => "cancel",
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
        if let Some(value) = &self.mask_overlay {
            object.insert("mask_overlay".into(), Value::from(value.clone()));
        }
        if let Some(value) = &self.mask_overlay_colour {
            object.insert("mask_overlay_colour".into(), Value::from(value.clone()));
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
            Self::AngleRail(fractions) => json!({"angle_rail":fractions}),
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
    /// The mask overlay's coverage grid must reach the GPU, for the same reason. It rides the
    /// frame the preview worker renders, so the frame lands first and the grid's own texture a
    /// message later; settling on the frame would capture the photograph without the overlay.
    MaskOverlay,
    /// The pointer readout must come back from `render.sample`.
    Readout,
    /// A canvas pick has reached an outcome that commits nothing: filled coordinates, or a refusal
    /// with its reason in the status bar. A pick that does commit re-arms [`Settle::Preview`]
    /// instead, so its frame is the committed render.
    Pick,
    /// A preset library call and the listing after it answered, or the call was refused.
    Presets,
    /// A host method a script called directly answered.
    Host,
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
            Step::Controls(control) => self.controls_step(control),
            Step::Picker(picker) => self.picker_step(picker),
            Step::Curve(curve) => self.curve_step(curve),
            Step::Group(group) => self.group_step(group),
            Step::Tab(tab) => self.tab_step(tab),
            Step::Section(section) => self.section_step(section),
            Step::Gallery(page) => self.gallery_step(page),
            Step::ToolsScroll(fraction) => self.tools_scroll_step(fraction),
            Step::Field(field) => self.field_step(field),
            Step::Reset(reset) => self.reset_step(reset),
            Step::Pick(pick) => self.pick_step(pick),
            Step::SliderDraft(decision) => self.slider_draft_step(decision),
            Step::View(view) => self.view_step(view),
            Step::Workspace(workspace) => self.workspace_step(workspace),
            Step::Preview(preview) => self.preview_step(preview),
            Step::Palette(palette) => self.palette_step(palette),
            Step::Hover { x, y } => self.hover_step(x, y),
            Step::Preset(pick) => self.preset_step(pick),
            Step::PresetCreate(step) => self.preset_create_step(step),
            Step::PresetDelete(pick) => self.preset_delete_step(pick),
            Step::PresetImport(path) => self.preset_import_step(path),
            Step::Mask(step) => self.mask_step(step),
        }
    }

    /// What a Masks-panel step waits for: the coverage grid's own texture when the overlay is on —
    /// settling on the frame would capture the photograph before the grid it is evidence of reached
    /// the GPU — and the frame itself when it is off.
    fn mask_settle(&self) -> Settle {
        if self.mask_overlay_request().is_some() {
            Settle::MaskOverlay
        } else {
            Settle::Preview
        }
    }

    /// One owner request with the desktop's own envelope: the current revision and a fresh request
    /// id, exactly as a control would send it. The frame is captured when its pixels arrive.
    ///
    /// A method that takes no mutation envelope, such as `preset.list` or `session.state`, is sent
    /// as written instead, with the open asset's identity only when it names one, and its frame is
    /// captured when it answers. Which kind a method is comes from the method table's own schema.
    fn api_step(&mut self, method: String, mut params: Map<String, Value>) -> Task<Message> {
        if let Err(reason) = self.resolve_identities(&mut params) {
            return self.fail_step(reason);
        }
        if let Some(takes_asset) = envelope_free(&method) {
            if takes_asset && let Some(state) = &self.state {
                params.insert("asset_id".into(), json!(state.asset.id));
            }
            self.await_step(Settle::Host);
            return host_task(
                self.owner.clone(),
                self.client,
                method,
                Value::Object(params),
            );
        }
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

    /// Replace a `{"name": …}` reference in a request's `mask` or `component` envelope field with
    /// the identity the host assigned to it, and record both beside the step.
    ///
    /// This is what lets one script create a mask and then edit through it: `mask.create-<kind>`
    /// assigns the identity, so the step that follows has nothing to write down but the name.
    /// Resolution reads the `mask.list` answer the editor is already holding, which the commit of
    /// every mutation refreshes, so a name resolves against the same listing the panel shows.
    fn resolve_identities(&mut self, params: &mut Map<String, Value>) -> Result<(), String> {
        let mut resolved = Map::new();
        let mut mask: Option<String> = None;
        if let Some(value) = params.get("mask").cloned() {
            let id = self.resolve_mask(&Reference::parse(&value, "mask")?)?;
            resolved.insert("mask".into(), json!(id));
            params.insert("mask".into(), json!(id));
            mask = Some(id);
        }
        if let Some(value) = params.get("component").cloned() {
            let reference = Reference::parse(&value, "component")?;
            let id = self.resolve_component(mask.as_deref(), &reference)?;
            resolved.insert("component".into(), json!(id));
            params.insert("component".into(), json!(id));
        }
        if !resolved.is_empty() {
            self.note_step(json!({ "resolved": resolved }));
        }
        Ok(())
    }

    /// One mask's identity, by identity or by the name the host gave it.
    fn resolve_mask(&self, reference: &Reference) -> Result<String, String> {
        if let Reference::Id(id) = reference {
            return Ok(id.clone());
        }
        let listing = self
            .masks
            .as_ref()
            .ok_or("no mask listing has been read yet")?;
        let name = match reference {
            Reference::Index(index) => {
                return listing
                    .masks
                    .get(*index)
                    .map(|report| report.id.as_str().to_owned())
                    .ok_or_else(|| format!("the stack holds {} masks", listing.masks.len()));
            }
            Reference::Name(name) => name,
            Reference::Id(_) => unreachable!("an identity is answered above"),
        };
        let mut found = listing.masks.iter().filter(|report| report.name == *name);
        let first = found
            .next()
            .ok_or_else(|| format!("no mask is named {name}"))?;
        if found.next().is_some() {
            return Err(format!("more than one mask is named {name}"));
        }
        Ok(first.id.as_str().to_owned())
    }

    /// One component's identity within a mask: the one the request names, else the open one.
    fn resolve_component(
        &self,
        mask: Option<&str>,
        reference: &Reference,
    ) -> Result<String, String> {
        if let Reference::Id(id) = reference {
            return Ok(id.clone());
        }
        let listing = self
            .masks
            .as_ref()
            .ok_or("no mask listing has been read yet")?;
        let mask = mask
            .map(str::to_owned)
            .or_else(|| self.selected_mask.as_ref().map(|id| id.as_str().to_owned()))
            .ok_or("a component named by name or position needs a mask, named or open")?;
        let report = listing
            .masks
            .iter()
            .find(|report| report.id.as_str() == mask)
            .ok_or_else(|| format!("no mask {mask} is listed"))?;
        let name = match reference {
            Reference::Index(index) => {
                return report
                    .components
                    .get(*index)
                    .map(|component| component.id.as_str().to_owned())
                    .ok_or_else(|| {
                        format!(
                            "{} holds {} components",
                            report.name,
                            report.components.len()
                        )
                    });
            }
            Reference::Name(name) => name,
            Reference::Id(_) => unreachable!("an identity is answered above"),
        };
        let mut found = report
            .components
            .iter()
            .filter(|component| component.name == *name);
        let first = found
            .next()
            .ok_or_else(|| format!("{} holds no component named {name}", report.name))?;
        if found.next().is_some() {
            return Err(format!(
                "{} holds more than one component named {name}",
                report.name
            ));
        }
        Ok(first.id.as_str().to_owned())
    }

    /// One stroke of a brush component, by its content address, by the label its row shows —
    /// `Stroke 1` — or by its position in that row's own list.
    ///
    /// A stroke is minted by the run that painted it, so a script written before the run has only
    /// the label or the position to write down, exactly as it has for a mask and a component. The
    /// list is the panel's own, which is filled while the row is open, so a script that names a
    /// stroke on a closed row is told to open it rather than being guessed at.
    fn resolve_stroke(&self, component: &str, reference: &Reference) -> Result<String, String> {
        if let Reference::Id(id) = reference {
            return Ok(id.clone());
        }
        let row = self
            .workspace
            .masks
            .components
            .iter()
            .find(|row| row.id.as_str() == component)
            .ok_or_else(|| format!("no component {component} is listed"))?;
        if row.strokes.is_empty() {
            return Err(format!(
                "{} lists no strokes; select the row first so its strokes are listed",
                row.name
            ));
        }
        let name = match reference {
            Reference::Index(index) => {
                return row
                    .strokes
                    .get(*index)
                    .map(|stroke| stroke.stroke.clone())
                    .ok_or_else(|| format!("{} holds {} strokes", row.name, row.strokes.len()));
            }
            Reference::Name(name) => name,
            Reference::Id(_) => unreachable!("an identity is answered above"),
        };
        let mut found = row.strokes.iter().filter(|stroke| stroke.label == *name);
        let first = found
            .next()
            .ok_or_else(|| format!("{} holds no stroke named {name}", row.name))?;
        if found.next().is_some() {
            return Err(format!(
                "{} holds more than one stroke named {name}",
                row.name
            ));
        }
        Ok(first.stroke.clone())
    }

    /// One Masks-panel view or mask-canvas gesture, through the same [`MaskMessage`] the panel's
    /// rows, buttons and the canvas raise. Geometry arrives in normalized content coordinates,
    /// which is what the canvas publishes once it has mapped the pointer through
    /// `render.transform`'s affine.
    ///
    /// A gesture that changes the drafted or committed picture settles on that picture's own
    /// pixels, and on the coverage grid's own texture while the overlay is on — settling on the
    /// frame would capture the photograph before the grid it is evidence of reached the GPU. One
    /// that only changes a selection is captured on the next redraw.
    fn mask_step(&mut self, step: MaskStep) -> Task<Message> {
        use crate::app::message::MaskPointer;
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if !self.mask_mode_active() {
            return self.fail_step("a mask step needs Mask mode");
        }
        let overlay = self.mask_overlay_request().is_some();
        // A drag is several messages; every other gesture is exactly one.
        if let MaskStep::Drag { handle, points } = &step {
            let Some(handle) = mask_handle(handle) else {
                return self.fail_step(format!("unknown mask handle {handle}"));
            };
            let Some((first, rest)) = points.split_first() else {
                return self.fail_step("a mask drag needs at least one point");
            };
            if self.mask_draft.is_none() {
                return self.fail_step("no mask gesture is open to drag");
            }
            let mut tasks = vec![self.mask_message(MaskMessage::Handle(MaskPointer::Begin {
                handle,
                x: first[0],
                y: first[1],
            }))];
            for point in rest {
                tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::Drag {
                    x: point[0],
                    y: point[1],
                })));
            }
            tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::End)));
            self.await_step(self.mask_settle());
            self.note_step(json!({"masks": self.workspace.masks.summary()}));
            return Task::batch(tasks);
        }
        // What must be true after the message for the frame the step waits for to ever arrive. A
        // gesture the editor refused raises no round trip, so its refusal is recorded with its own
        // frame rather than leaving the run waiting for pixels nothing will render.
        enum Expect {
            /// Nothing is in flight; the frame is the next redraw.
            Redraw,
            /// The overlay is what changes, and nothing can refuse it.
            Overlay,
            /// A gesture must now be open.
            Gesture,
            /// A round trip must now be in flight.
            RoundTrip,
            /// The panel must have sent the row's own command.
            Request,
        }
        let hovering = if overlay {
            Expect::Overlay
        } else {
            Expect::Redraw
        };
        let (message, expect) = match step {
            MaskStep::Select(reference) => match self.resolve_mask(&reference) {
                Ok(id) => (MaskMessage::Select(id), Expect::Redraw),
                Err(reason) => return self.fail_step(reason),
            },
            // A selection opens that row's own numbers and renders nothing: the overlay follows the
            // pointer, not the selection, so the step is captured on the next frame rather than
            // waiting for pixels nothing asked for.
            MaskStep::SelectComponent(Some(reference)) => {
                match self.resolve_component(None, &reference) {
                    Ok(id) => (MaskMessage::SelectComponent(id), Expect::Redraw),
                    Err(reason) => return self.fail_step(reason),
                }
            }
            MaskStep::SelectComponent(None) => {
                self.selected_component = None;
                self.seed_mask_fields();
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                self.capture_next_frame();
                return Task::none();
            }
            MaskStep::Hover(Some(reference)) => match self.resolve_component(None, &reference) {
                Ok(id) => (MaskMessage::Hover(Some(id)), hovering),
                Err(reason) => return self.fail_step(reason),
            },
            MaskStep::Hover(None) => (MaskMessage::Hover(None), hovering),
            MaskStep::EditShape(reference) => match self.resolve_component(None, &reference) {
                Ok(id) => (MaskMessage::EditShape(id), Expect::Gesture),
                Err(reason) => return self.fail_step(reason),
            },
            // Choosing the next component's mode changes no pixel and asks for nothing: it is the
            // Add row's own state, and its captured frame is the panel showing that choice.
            MaskStep::Mode(mode) => {
                let Some(index) = crate::state::masks::MODES
                    .iter()
                    .position(|known| known.as_str() == mode)
                else {
                    return self.fail_step(format!("no component mode is called {mode}"));
                };
                (MaskMessage::SetAddMode(index), Expect::Redraw)
            }
            // A kind with handles opens a gesture; a **typed** kind — one whose geometry is entirely
            // defaulted, as a range selection's is — is created straight away and so has a round
            // trip rather than a draft to wait for. The step reads the host's own declarations to
            // know which, exactly as the panel's button does, so registering a kind is still all it
            // takes for a script to reach it.
            MaskStep::New(kind) => {
                let expect = if mask_kind_is_typed(&kind) {
                    Expect::RoundTrip
                } else {
                    Expect::Gesture
                };
                (MaskMessage::New(kind), expect)
            }
            MaskStep::Add(kind) => {
                let expect = if mask_kind_is_typed(&kind) {
                    Expect::RoundTrip
                } else {
                    Expect::Gesture
                };
                (MaskMessage::Add(kind), expect)
            }
            // Arming the brush asks for no frame of its own: a stroke with no path is not a geometry
            // the host can preview, so the gesture waits for the pointer rather than for pixels
            // nothing requested, and the step is captured on the next frame.
            MaskStep::Paint(target) => {
                let target = match target {
                    PaintStep::NewMask => PaintTarget::NewMask,
                    PaintStep::NewBrush => PaintTarget::NewBrush,
                    PaintStep::Component(reference) => {
                        match self.resolve_component(None, &reference) {
                            Ok(id) => PaintTarget::Component(id),
                            Err(reason) => return self.fail_step(reason),
                        }
                    }
                };
                let task = self.mask_message(MaskMessage::Paint(target));
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                if self.mask_draft.is_none() {
                    let reason = self.status.clone();
                    return Task::batch([task, self.fail_step(reason)]);
                }
                self.capture_next_frame();
                return task;
            }
            // The brush changes no pixel and asks for nothing: it is the setting the next stroke
            // will be drawn with, and its captured frame is the panel showing that setting.
            MaskStep::Brush(brush) => {
                let mut tasks = Vec::new();
                for (name, value) in [
                    ("size", brush.size),
                    ("feather", brush.feather),
                    ("flow", brush.flow),
                    ("colour_refine", brush.colour_refine),
                ] {
                    if let Some(value) = value {
                        tasks.push(self.mask_message(MaskMessage::Brush(BrushEdit::Set {
                            name: name.to_owned(),
                            value,
                        })));
                    }
                }
                if let Some((name, steps)) = brush.nudge {
                    tasks.push(
                        self.mask_message(MaskMessage::Brush(BrushEdit::Nudge { name, steps })),
                    );
                }
                if let Some(erase) = brush.erase {
                    tasks.push(self.mask_message(MaskMessage::Brush(BrushEdit::Erase(erase))));
                }
                if let Some(held) = brush.erase_held {
                    tasks.push(self.mask_message(MaskMessage::Brush(BrushEdit::EraseHeld(held))));
                }
                if let Some(limit) = brush.limit_to_colour {
                    tasks.push(
                        self.mask_message(MaskMessage::Brush(BrushEdit::LimitToColour(limit))),
                    );
                }
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                self.capture_next_frame();
                return Task::batch(tasks);
            }
            // One whole stroke: a press, a move per position and, unless the step leaves it down,
            // the release that commits it as one history entry. The brush stays in hand afterwards,
            // so the next stroke needs no further `paint` and Apply has nothing left to commit.
            MaskStep::Stroke { points, release } => {
                if self
                    .mask_draft
                    .as_ref()
                    .and_then(MaskDraft::brush)
                    .is_none()
                {
                    return self.fail_step("no painted gesture is open to paint into");
                }
                let Some((first, rest)) = points.split_first() else {
                    return self.fail_step("a stroke needs at least one position");
                };
                let mut tasks =
                    vec![
                        self.mask_message(MaskMessage::Handle(MaskPointer::PaintBegin {
                            x: first[0],
                            y: first[1],
                        })),
                    ];
                for point in rest {
                    tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::PaintTo {
                        x: point[0],
                        y: point[1],
                    })));
                }
                if release {
                    tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::PaintEnd)));
                }
                self.await_step(self.mask_settle());
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                return Task::batch(tasks);
            }
            MaskStep::Sweep { from, to } => {
                if self.mask_draft.is_none() {
                    return self.fail_step("no mask gesture is open to sweep");
                }
                (
                    MaskMessage::Handle(MaskPointer::Sweep {
                        from: (from[0], from[1]),
                        to: (to[0], to[1]),
                    }),
                    Expect::Gesture,
                )
            }
            MaskStep::Release => {
                if self.mask_draft.is_none() {
                    return self.fail_step("no mask gesture is open to release");
                }
                (MaskMessage::Handle(MaskPointer::End), Expect::Gesture)
            }
            // The host's own pick, entered and left the way the panel's button does: one
            // `workspace.set` and nothing committed, so the frame after it shows the mode.
            MaskStep::Pick => {
                if self.selected_component.is_none() {
                    return self.fail_step("a pick step needs a selected component to fill");
                }
                // Entering or leaving a pick mode commits nothing and changes no pixel, so the
                // frame is the next redraw rather than a preview that will never arrive.
                (MaskMessage::Pick, Expect::Redraw)
            }
            MaskStep::Apply => {
                if self.mask_draft.is_none() {
                    return self.fail_step("no mask gesture is open to apply");
                }
                (MaskMessage::Apply, Expect::RoundTrip)
            }
            MaskStep::Cancel => {
                if self.mask_draft.is_none() {
                    return self.fail_step("no mask gesture is open to cancel");
                }
                (MaskMessage::Cancel, Expect::RoundTrip)
            }
            MaskStep::Row {
                component: at,
                edit,
            } => {
                let id = match self.resolve_component(None, &at) {
                    Ok(id) => id,
                    Err(reason) => return self.fail_step(reason),
                };
                (
                    MaskMessage::Row(match edit {
                        RowStep::Mode(mode) => RowEdit::ComponentMode {
                            component: id,
                            mode,
                        },
                        RowStep::Invert(invert) => RowEdit::ComponentInvert {
                            component: id,
                            invert,
                        },
                        RowStep::Move(index) => RowEdit::MoveComponent {
                            component: id,
                            index,
                        },
                        RowStep::Delete => RowEdit::DeleteComponent(id),
                        // A stroke is resolved against the row's own list, which is the list the
                        // panel's delete button reads too.
                        RowStep::DeleteStroke(stroke) => match self.resolve_stroke(&id, &stroke) {
                            Ok(stroke) => RowEdit::DeleteStroke {
                                component: id,
                                stroke,
                            },
                            Err(reason) => return self.fail_step(reason),
                        },
                    }),
                    Expect::Request,
                )
            }
            MaskStep::Drag { .. } => unreachable!("a drag is answered above"),
        };
        if matches!(expect, Expect::Redraw) {
            self.capture_next_frame();
            let task = self.mask_message(message);
            self.note_step(json!({"masks": self.workspace.masks.summary()}));
            return task;
        }
        // A row edit the panel refuses sends nothing, so the step would wait for a frame nothing
        // arms. Whether one went out is read from the request the panel records as it sends it,
        // which is also what the refusal replaces.
        self.last_mask_request = None;
        self.await_step(self.mask_settle());
        let task = self.mask_message(message);
        let armed = match expect {
            Expect::Redraw | Expect::Overlay => true,
            // An open gesture always has a round trip of its own: `draft.begin` while it is
            // opening, `draft.set` once it has, and a drafted frame at the end of either.
            Expect::Gesture => self.mask_draft.is_some(),
            Expect::RoundTrip => {
                self.mask_draft_in_flight
                    || self.mask_draft_pending
                    || self.mask_draft_finish
                    || self.mask_draft.is_none()
            }
            Expect::Request => self.last_mask_request.is_some(),
        };
        self.note_step(json!({"masks": self.workspace.masks.summary()}));
        if armed {
            return task;
        }
        // The refusal's own reason is the evidence, captured on the frame that is on screen.
        let reason = self.status.clone();
        Task::batch([task, self.fail_step(reason)])
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
            DraftStep::AngleRail(fractions) => {
                if !drafting {
                    return self.fail_step("no crop draft is open");
                }
                let mut tasks: Vec<Task<Message>> = fractions
                    .iter()
                    .map(|fraction| self.crop_update(CropMessage::AngleRail(*fraction)))
                    .collect();
                tasks.push(self.crop_update(CropMessage::AngleRailReleased));
                self.capture_next_frame();
                return Task::batch(tasks);
            }
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
    /// per value, then the release, Escape or nothing at all. Each move sends its own `draft.set`
    /// when nothing is in flight, so the `SliderDraftTick` after it normally finds nothing to do.
    /// Nothing here reaches the owner directly; the gesture's own driver does, under its own bound.
    ///
    /// A step with `interval_ms` sends nothing here: it hands its values to
    /// [`Editor::slider_paced_tick`] instead, one per tick of the timer the subscription starts
    /// while `paced_slider` holds them, so this function's own frame is never captured for it.
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
        if let Some(interval_ms) = step.interval_ms {
            if let Some(evidence) = &mut self.evidence {
                evidence.paced_slider = Some(PacedSlider {
                    action: step.action,
                    parameter: step.parameter,
                    remaining: step.values.into(),
                    sent: 0,
                    interval_ms,
                    end: step.end,
                });
            }
            return Task::none();
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
        tasks.push(self.end_slider_gesture(step.action, step.parameter, step.end));
        Task::batch(tasks)
    }

    /// One tick of a paced slider step: send its next value through the same messages a fast
    /// pointer drag sends, record it as its own event so the harness can time an input that never
    /// reaches the owner, and, on the last value, end the gesture exactly as the unpaced step does.
    /// A tick with nothing left to send, because no paced step is running or its last tick has
    /// already ended it, does nothing: the subscription that calls this exists only while
    /// `paced_slider` holds values, so that should not happen, but the message is harmless either
    /// way.
    pub(crate) fn slider_paced_tick(&mut self) -> Task<Message> {
        let Some(paced) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.paced_slider.as_mut())
        else {
            return Task::none();
        };
        let Some(value) = paced.remaining.pop_front() else {
            return Task::none();
        };
        let index = paced.sent;
        paced.sent += 1;
        let action = paced.action.clone();
        let parameter = paced.parameter.clone();
        let end = paced.end;
        let done = paced.remaining.is_empty();
        if done && let Some(evidence) = &mut self.evidence {
            evidence.paced_slider = None;
        }
        self.event("slider_step_value", json!({"value": value, "index": index}));
        let mut tasks = vec![
            self.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value,
            }),
            self.update(Message::SliderDraftTick),
        ];
        if done {
            if self.slider_draft.is_none() && end != SliderEnd::Cancel {
                return self.fail_step(format!(
                    "the {action} draft could not be opened: {}",
                    self.status
                ));
            }
            tasks.push(self.end_slider_gesture(action, parameter, end));
        }
        Task::batch(tasks)
    }

    /// End a slider gesture the way a step says to: released, cancelled, or left open. Shared by
    /// the unpaced and paced drivers so both settle exactly the same way.
    fn end_slider_gesture(
        &mut self,
        action: String,
        parameter: String,
        end: SliderEnd,
    ) -> Task<Message> {
        match end {
            // The committed pixels are the evidence, so this waits for the render the commit
            // produces; a return-to-start gesture settles the same step with no entry at all.
            SliderEnd::Release => {
                self.await_step(Settle::Preview);
                self.update(Message::SliderReleased { action, parameter })
            }
            // Escape, through the same message the keyboard table produces.
            SliderEnd::Cancel => {
                self.await_step(Settle::Preview);
                self.update(Message::SliderDraftCancel)
            }
            // Left open: the frame shows the drafted preview, captured once the gesture has
            // drained, so the pixels belong to the newest value it sent.
            SliderEnd::Open => {
                self.await_step(Settle::SliderDraft);
                Task::none()
            }
        }
    }

    /// Generated controls publish fractions and typed values, then use the same bounded draft
    /// driver as ordinary pointer input. The old `slider` step remains physical-value evidence.
    fn controls_step(&mut self, step: ControlsStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        match step {
            ControlsStep::Slider {
                action,
                parameter,
                fractions,
                finish,
            } => {
                let mut tasks = Vec::new();
                for fraction in fractions {
                    tasks.push(self.update(Message::ControlFraction {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        fraction,
                    }));
                    tasks.push(self.update(Message::SliderDraftTick));
                }
                self.finish_generated_gesture(
                    action,
                    parameter,
                    finish,
                    tasks,
                    GeneratedKind::Slider,
                )
            }
            ControlsStep::Discrete {
                action,
                parameter,
                value,
            } => {
                self.begin_request();
                let task = self.update(Message::ControlDiscrete {
                    action,
                    parameter,
                    value,
                });
                if !self.busy {
                    return self.fail_step(format!("the control did not submit: {}", self.status));
                }
                task
            }
        }
    }

    fn picker_step(&mut self, step: PickerStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let key = (step.action.clone(), step.parameter.clone());
        let current_open = self
            .controls_ui
            .color_open
            .get(&key)
            .copied()
            .unwrap_or(false);
        let mut tasks = Vec::new();
        if current_open != step.open.unwrap_or(true) {
            tasks.push(self.update(Message::TogglePicker {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
            }));
        }
        if let Some(hue) = step.hue {
            tasks.push(self.update(Message::ControlPicker {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                event: ColorPickerEvent::Hue(hue),
            }));
            tasks.push(self.update(Message::SliderDraftTick));
        }
        if let Some(plane) = step.plane {
            tasks.push(self.update(Message::ControlPicker {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                event: ColorPickerEvent::Plane(plane),
            }));
            tasks.push(self.update(Message::SliderDraftTick));
        }
        if step.hue.is_none() && step.plane.is_none() {
            self.capture_next_frame();
            return Task::batch(tasks);
        }
        self.finish_generated_gesture(
            step.action,
            step.parameter,
            step.finish,
            tasks,
            GeneratedKind::Picker,
        )
    }

    fn curve_step(&mut self, step: CurveStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mut tasks = Vec::new();
        match step.event {
            CurveStepEvent::Move { index, points } => {
                for position in points {
                    tasks.push(self.update(Message::ControlCurve {
                        action: step.action.clone(),
                        parameter: step.parameter.clone(),
                        event: CurveEditorEvent::Move { index, position },
                    }));
                    tasks.push(self.update(Message::SliderDraftTick));
                }
                self.finish_generated_gesture(
                    step.action,
                    step.parameter,
                    step.finish,
                    tasks,
                    GeneratedKind::Curve,
                )
            }
            CurveStepEvent::Add(point) => {
                self.begin_request();
                let task = self.update(Message::ControlCurve {
                    action: step.action,
                    parameter: step.parameter,
                    event: CurveEditorEvent::Add(point),
                });
                if !self.busy {
                    return self
                        .fail_step(format!("the curve point was not added: {}", self.status));
                }
                task
            }
            CurveStepEvent::Remove(index) => {
                self.begin_request();
                let task = self.update(Message::ControlCurve {
                    action: step.action,
                    parameter: step.parameter,
                    event: CurveEditorEvent::Remove(index),
                });
                if !self.busy {
                    return self
                        .fail_step(format!("the curve point was not removed: {}", self.status));
                }
                task
            }
            CurveStepEvent::Channel(index) => {
                let task = self.update(Message::ControlCurve {
                    action: step.action.clone(),
                    parameter: step.parameter.clone(),
                    event: CurveEditorEvent::Channel(index),
                });
                if selected_curve_channel(&self.workspace.tools, &step.action, &step.parameter)
                    != Some(index)
                {
                    return self.fail_step("the declared curve channel was not selected");
                }
                self.capture_next_frame();
                task
            }
        }
    }

    fn finish_generated_gesture(
        &mut self,
        action: String,
        parameter: String,
        finish: SliderEnd,
        mut tasks: Vec<Task<Message>>,
        kind: GeneratedKind,
    ) -> Task<Message> {
        if !self
            .slider_draft
            .as_ref()
            .is_some_and(|draft| draft.action == action && draft.parameter == parameter)
        {
            return self.fail_step(format!(
                "the {action} draft could not be opened: {}",
                self.status
            ));
        }
        match finish {
            SliderEnd::Open => self.await_step(Settle::SliderDraft),
            SliderEnd::Release => {
                self.await_step(Settle::Preview);
                let release = match kind {
                    GeneratedKind::Slider => Message::ControlReleased { action, parameter },
                    GeneratedKind::Picker => Message::ControlPicker {
                        action,
                        parameter,
                        event: ColorPickerEvent::Release,
                    },
                    GeneratedKind::Curve => Message::ControlCurve {
                        action,
                        parameter,
                        event: CurveEditorEvent::Release,
                    },
                };
                tasks.push(self.update(release));
            }
            SliderEnd::Cancel => {
                self.await_step(Settle::Preview);
                tasks.push(self.update(Message::SliderDraftCancel));
            }
        }
        Task::batch(tasks)
    }

    fn group_step(&mut self, step: GroupStep) -> Task<Message> {
        let Some(initial) =
            crate::app::controls::initial_group_expanded(&self.modules, &step.module, &step.path)
        else {
            return self.fail_step("the module declares no control group at that path");
        };
        let key = crate::state::tools::group_key(&step.module, &step.path);
        let expanded = self
            .controls_ui
            .group_expanded
            .get(&key)
            .copied()
            .unwrap_or(initial);
        let task = if expanded == step.expanded {
            Task::none()
        } else {
            self.update(Message::ToggleGroup {
                module_id: step.module,
                path: step.path,
            })
        };
        self.capture_next_frame();
        task
    }

    fn tab_step(&mut self, step: TabStep) -> Task<Message> {
        let tabbed = self.modules.iter().any(|module| {
            module.id == step.module && module.layout == lightwell_core::ModuleLayout::Tabs
        });
        if !tabbed {
            return self.fail_step("the module declares no tabbed layout");
        }
        let task = self.update(Message::SelectTab {
            module_id: step.module,
            index: step.index,
        });
        self.capture_next_frame();
        task
    }

    fn section_step(&mut self, step: SectionStep) -> Task<Message> {
        let Some(section) = self
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == step.module)
        else {
            return self.fail_step(format!("no section for {}", step.module));
        };
        let task = if section.expanded == step.expanded {
            Task::none()
        } else {
            self.update(Message::ToggleSection(step.module))
        };
        self.capture_next_frame();
        task
    }

    fn gallery_step(&mut self, page: Option<usize>) -> Task<Message> {
        if !self.developer
            || page.is_some_and(|page| crate::view::gallery_page_info(page).is_none())
        {
            return self.fail_step("gallery requires developer mode and an existing page");
        }
        if page.is_some() && !self.workspace.title.can_open_gallery {
            return self.fail_step("gallery cannot interrupt the current operation");
        }
        self.await_step(Settle::Session);
        self.update(Message::Gallery(page))
    }

    fn tools_scroll_step(&mut self, fraction: f64) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.tools_scroll = Some(fraction);
        }
        self.capture_next_frame();
        iced::widget::operation::snap_to(
            crate::view::tools_panel::scroll_id(),
            iced::widget::scrollable::RelativeOffset {
                x: 0.0,
                y: fraction as f32,
            },
        )
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

    /// One click on the photograph, at a pixel of the raster on screen, exactly as the canvas
    /// publishes it. What the click means is the active canvas mode's own declared pick: a point
    /// pick fills that mode's coordinate fields and commits nothing, and a sample-apply pick runs
    /// its module's query and submits the answer once. Nothing here names either.
    fn pick_step(&mut self, step: PickStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mode = self.session.workspace.mode.clone();
        if crate::state::tools::canvas_pick(&self.modules, &mode).is_none() {
            return self.fail_step(format!("the {mode} canvas mode declares no pick"));
        }
        if let Some(reason) = self.pick_refusal() {
            return self.fail_step(reason);
        }
        self.await_step(Settle::Pick);
        self.update(Message::PointPicked {
            x: step.x,
            y: step.y,
        })
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
        // The mask overlay is the same kind of view state, and its grid arrives beside the next
        // frame rather than from a derivation of its own, so the step settles on that frame.
        let mut mask_overlay = false;
        for (field, wanted, current) in [
            (
                "mask_overlay",
                step.mask_overlay.as_deref(),
                workspace.mask_overlay.as_str(),
            ),
            (
                "mask_overlay_colour",
                step.mask_overlay_colour.as_deref(),
                workspace.mask_overlay_colour.as_str(),
            ),
        ] {
            if let Some(value) = wanted
                && value != current
            {
                diff.insert(field.into(), Value::from(value));
                mask_overlay = true;
            }
        }
        // A grid only rides the next frame when the overlay will actually draw one: the mode it is
        // left in is not `off`, Mask mode is the canvas mode and a mask is open. Switching the
        // overlay off, or switching it on with nothing to draw, still asks for the frame — so the
        // step settles on those pixels rather than on a texture that will never be uploaded.
        let leaving_on = step
            .mask_overlay
            .as_deref()
            .unwrap_or(workspace.mask_overlay.as_str())
            != lightwell_core::MaskOverlayMode::Off.as_str();
        let entering_mask_mode =
            step.mode.as_deref().unwrap_or(workspace.mode.as_str()) == lightwell_core::MASK_MODE;
        let grid_expected = leaving_on && entering_mask_mode && self.selected_mask.is_some();
        if diff.is_empty() {
            self.capture_next_frame();
            return Task::none();
        }
        if mask_overlay {
            // The session answer is followed by one preview job, and — when there is a grid to
            // draw — its own texture a message after that, so the capture waits for the overlay's
            // pixels rather than for the frame they are drawn over. With nothing to draw, that
            // texture never arrives and the frame itself is what the step waits for.
            self.await_step(if grid_expected {
                Settle::MaskOverlay
            } else {
                Settle::Preview
            });
            let session = workspace_task(self.owner.clone(), self.client, Value::Object(diff));
            let frame = self.refresh_mask_overlay();
            return Task::batch([session, frame]);
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

    /// Click one row, exactly as the section does: the section's own action with that preset's
    /// fields, through the action path every declared control takes. Captured on the render.
    fn preset_step(&mut self, pick: PresetPick) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let row = match self.preset_row(&pick) {
            Ok(row) => row,
            Err(reason) => return self.fail_step(reason),
        };
        let presets = self
            .workspace
            .tools
            .all()
            .find_map(|section| section.presets())
            .cloned()
            .unwrap_or_default();
        let Some(preset) = row.apply.clone().filter(|_| row.enabled) else {
            let reason = row
                .unavailable
                .clone()
                .or(presets.apply_disabled)
                .unwrap_or_else(|| "the row is disabled".into());
            return self.fail_step(format!("{} cannot apply: {reason}", pick.name));
        };
        self.note_step(json!({"preset_id":row.id}));
        self.begin_request();
        let task = self.update(Message::RunAction {
            action: presets.action,
            preset,
        });
        if !self.busy {
            return self.fail_step(format!("the preset was not applied: {}", self.status));
        }
        task
    }

    /// Fill and submit the create form through its own messages, in the order a person would.
    fn preset_create_step(&mut self, step: PresetCreateStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let labels: Vec<String> = presettable_groups(&self.modules, self.developer)
            .into_iter()
            .map(|group| group.label)
            .collect();
        if let Some(unknown) = step.groups.iter().find(|label| !labels.contains(label)) {
            return self.fail_step(format!("no create-form group is labelled {unknown}"));
        }
        let mut tasks = Vec::new();
        if !self.preset_form.open {
            tasks.push(self.update(Message::Preset(PresetMessage::ToggleForm)));
        }
        tasks.push(self.update(Message::Preset(PresetMessage::Name(step.name))));
        if let Some(group) = step.group {
            tasks.push(self.update(Message::Preset(PresetMessage::Group(group))));
        }
        for label in labels {
            let checked = step.groups.contains(&label);
            tasks.push(self.update(Message::Preset(PresetMessage::Check { label, checked })));
        }
        if !step.submit {
            self.capture_next_frame();
            return Task::batch(tasks);
        }
        self.await_step(Settle::Presets);
        tasks.push(self.update(Message::Preset(PresetMessage::Create)));
        Task::batch(tasks)
    }

    /// Delete one preset through its row's menu: open the menu on the row, then choose Delete.
    fn preset_delete_step(&mut self, pick: PresetPick) -> Task<Message> {
        let row = match self.preset_row(&pick) {
            Ok(row) => row,
            Err(reason) => return self.fail_step(reason),
        };
        self.note_step(json!({"preset_id":row.id}));
        let open = self.update(Message::OpenMenu(MenuTarget::Preset(row.id.clone())));
        self.await_step(Settle::Presets);
        let delete = self.update(Message::Preset(PresetMessage::Delete(row.id)));
        Task::batch([open, delete])
    }

    /// Import one file through the same task the dialog's answer starts.
    fn preset_import_step(&mut self, path: String) -> Task<Message> {
        self.await_step(Settle::Presets);
        self.preset_import(PathBuf::from(path))
    }

    /// The one row a step names: the exact name, and the group when the step gives one.
    fn preset_row(&self, pick: &PresetPick) -> Result<PresetRow, String> {
        let presets = self
            .workspace
            .tools
            .all()
            .find_map(|section| section.presets())
            .ok_or("no module declares a presets control")?;
        let matches: Vec<&PresetRow> = presets
            .rows()
            .filter(|row| row.name == pick.name)
            .filter(|row| pick.group.as_ref().is_none_or(|group| &row.group == group))
            .collect();
        let named = match &pick.group {
            Some(group) => format!("{} in {group}", pick.name),
            None => pick.name.clone(),
        };
        match matches.as_slice() {
            [row] => Ok((*row).clone()),
            [] => Err(format!("no preset is named {named}")),
            many => Err(format!(
                "{} presets are named {named}; name its group",
                many.len()
            )),
        }
    }

    /// A host method the running step called answered: adopt the library it listed, record what it
    /// said, and capture the frame.
    pub(crate) fn host_answered(&mut self, result: Result<HostAnswer, String>) {
        match result {
            Ok(answer) => {
                if let Some(presets) = answer.presets {
                    self.adopt_presets(presets, answer.sequence);
                }
                self.status = format!("{} answered", answer.method);
                self.note_step(json!({"result":answer.result}));
            }
            Err(error) => {
                self.refuse_step(&error);
                self.status = error;
            }
        }
        self.settle_step(Settle::Host);
    }

    /// A request the running step sent was refused. The refusal still captures a frame, so it is
    /// recorded on the step and on the run rather than passing for a success.
    pub(crate) fn refuse_step(&mut self, reason: &str) {
        if self
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.current.is_none())
        {
            return;
        }
        self.event("script_step_failed", json!({"reason":reason}));
        self.note_step(json!({"status":"failed","reason":reason}));
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors = true;
        }
    }

    /// The running step waits for this before its frame is captured.
    pub(crate) fn await_step(&mut self, settle: Settle) {
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
    pub(crate) fn note_step(&mut self, detail: Value) {
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
    let object = step
        .as_object()
        .ok_or("a step is an object with one recognized step key")?;
    let (kind, value) = sole(object)?;
    match kind {
        "api" => parse_api(value),
        "draft" => Ok(Step::Draft(parse_draft(value)?)),
        "slider" => Ok(Step::Slider(parse_slider(value)?)),
        "controls" => Ok(Step::Controls(parse_controls(value)?)),
        "picker" => Ok(Step::Picker(parse_picker(value)?)),
        "curve" => Ok(Step::Curve(parse_curve(value)?)),
        "group" => Ok(Step::Group(parse_group(value)?)),
        "tab" => Ok(Step::Tab(parse_tab(value)?)),
        "section" => Ok(Step::Section(parse_section(value)?)),
        "gallery" => Ok(Step::Gallery(parse_gallery(value)?)),
        "tools_scroll" => Ok(Step::ToolsScroll(unit_number(value, "tools_scroll")?)),
        "slider_draft" => Ok(Step::SliderDraft(parse_slider_draft(value)?)),
        "field" => Ok(Step::Field(parse_field_step(value)?)),
        "reset" => Ok(Step::Reset(parse_reset(value)?)),
        "pick" => Ok(Step::Pick(parse_pick(value)?)),
        "view" => Ok(Step::View(parse_view(value)?)),
        "workspace" => Ok(Step::Workspace(parse_workspace(value)?)),
        "preview" => Ok(Step::Preview(parse_preview(value)?)),
        "palette" => Ok(Step::Palette(parse_palette(value)?)),
        "hover" => parse_hover(value),
        "preset" => Ok(Step::Preset(parse_preset_pick(value, "preset")?)),
        "preset_create" => Ok(Step::PresetCreate(parse_preset_create(value)?)),
        "preset_delete" => Ok(Step::PresetDelete(parse_preset_pick(
            value,
            "preset_delete",
        )?)),
        "preset_import" => parse_preset_import(value),
        "mask" => Ok(Step::Mask(parse_mask(value)?)),
        other => Err(format!(
            "unknown step kind {other}; expected api, draft, slider, controls, picker, curve, group, tab, section, gallery, tools_scroll, slider_draft, field, reset, pick, view, workspace, preview, palette, hover, preset, preset_create, preset_delete, preset_import or mask"
        )),
    }
}

/// The handle names a script may write, for the refusal that lists them. The create gesture's own
/// grab is not among them: it is not drawn, and a sweep is how a script uses it.
const MASK_HANDLES: &str =
    "start, middle, end, centre, radius+x, radius-x, radius+y, radius-y, rotation or feather";

/// One drawn handle of a gesture's figure, by the name the design draws it under.
fn mask_handle(name: &str) -> Option<crate::mask_draft::MaskHandle> {
    use crate::mask_draft::MaskHandle;
    match name {
        "start" => Some(MaskHandle::Start),
        "middle" => Some(MaskHandle::Middle),
        "end" => Some(MaskHandle::End),
        "centre" => Some(MaskHandle::Centre),
        "radius+x" => Some(MaskHandle::RadiusPlusX),
        "radius-x" => Some(MaskHandle::RadiusMinusX),
        "radius+y" => Some(MaskHandle::RadiusPlusY),
        "radius-y" => Some(MaskHandle::RadiusMinusY),
        "rotation" => Some(MaskHandle::Rotation),
        "feather" => Some(MaskHandle::Feather),
        _ => None,
    }
}

/// One `mask` step: `{"mask": {"select": 0}}`, `{"mask": {"hover": null}}`,
/// `{"mask": {"select_component": {"name": "Linear 1"}}}`,
/// `{"mask": {"sweep": {"from": [x, y], "to": [x, y]}}}`, `{"mask": {"row": {…}}}` or
/// `{"mask": {"cancel": true}}`. Exactly one verb per step, so a step's own captured frame is
/// evidence of one thing.
fn parse_mask(value: &Value) -> Result<MaskStep, String> {
    let object = value
        .as_object()
        .ok_or("mask takes an object with exactly one key")?;
    let (key, value) = sole(object)?;
    let kind = || {
        value
            .as_str()
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("mask {key} takes a component kind"))
    };
    let word = || {
        value
            .as_str()
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("mask {key} takes a name"))
    };
    let requested = || match value.as_bool() {
        Some(true) => Ok(()),
        _ => Err(format!("mask {key} takes true")),
    };
    // Naming nothing is what clears a selection and what takes the pointer off the list, so those
    // two verbs take `null` where the others need a row.
    let optional = || match value {
        Value::Null => Ok(None),
        other => Reference::parse(other, "component").map(Some),
    };
    match key {
        "select" => Ok(MaskStep::Select(Reference::parse(value, "mask")?)),
        "select_component" => Ok(MaskStep::SelectComponent(optional()?)),
        "hover" => Ok(MaskStep::Hover(optional()?)),
        "edit_shape" => Ok(MaskStep::EditShape(Reference::parse(value, "component")?)),
        "mode" => Ok(MaskStep::Mode(word()?)),
        "new" => Ok(MaskStep::New(kind()?)),
        "add" => Ok(MaskStep::Add(kind()?)),
        "pick" => {
            if value.as_bool() == Some(true) {
                Ok(MaskStep::Pick)
            } else {
                Err("mask pick takes true".to_owned())
            }
        }
        "paint" => parse_paint(value),
        "brush" => parse_brush(value).map(MaskStep::Brush),
        "stroke" => parse_stroke(value),
        "sweep" => {
            let object = value
                .as_object()
                .ok_or("mask sweep takes an object with from and to")?;
            for name in object.keys() {
                if !matches!(name.as_str(), "from" | "to") {
                    return Err(format!("unknown mask sweep field {name}"));
                }
            }
            Ok(MaskStep::Sweep {
                from: content_point(object.get("from"), "mask sweep from")?,
                to: content_point(object.get("to"), "mask sweep to")?,
            })
        }
        "drag" => {
            let object = value
                .as_object()
                .ok_or("mask drag takes an object with a handle and its points")?;
            for name in object.keys() {
                if !matches!(name.as_str(), "handle" | "points") {
                    return Err(format!("unknown mask drag field {name}"));
                }
            }
            let handle = object
                .get("handle")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("mask drag needs a handle of {MASK_HANDLES}"))?;
            if mask_handle(handle).is_none() {
                return Err(format!(
                    "unknown mask handle {handle}; expected {MASK_HANDLES}"
                ));
            }
            let points = object
                .get("points")
                .and_then(Value::as_array)
                .ok_or("mask drag needs a points array")?
                .iter()
                .map(|point| content_point(Some(point), "mask drag point"))
                .collect::<Result<Vec<_>, _>>()?;
            if points.is_empty() {
                return Err("mask drag needs at least one point".into());
            }
            Ok(MaskStep::Drag {
                handle: handle.to_owned(),
                points,
            })
        }
        "release" => requested().map(|()| MaskStep::Release),
        "apply" => requested().map(|()| MaskStep::Apply),
        "cancel" => requested().map(|()| MaskStep::Cancel),
        "row" => parse_mask_row(value),
        other => Err(format!(
            "unknown mask step {other}; expected select, select_component, hover, edit_shape, mode, new, add, paint, brush, stroke, sweep, release, drag, apply, cancel or row"
        )),
    }
}

/// `{"paint": "new-mask"}`, `{"paint": "new-brush"}` or `{"paint": {"component": REF}}`: what the
/// next stroke lands on. The Add row offers no brush, because a brush declares no geometry to type,
/// so this is the route a script reaches one by, exactly as the panel's Brush section is.
fn parse_paint(value: &Value) -> Result<MaskStep, String> {
    const SHAPE: &str = "mask paint takes new-mask, new-brush or {\"component\": REF}";
    match value {
        Value::String(word) => match word.trim() {
            "new-mask" => Ok(MaskStep::Paint(PaintStep::NewMask)),
            "new-brush" => Ok(MaskStep::Paint(PaintStep::NewBrush)),
            _ => Err(SHAPE.to_owned()),
        },
        Value::Object(object) => {
            let (key, value) = sole(object)?;
            if key != "component" {
                return Err(format!("unknown mask paint field {key}"));
            }
            Reference::parse(value, "component")
                .map(|component| MaskStep::Paint(PaintStep::Component(component)))
        }
        _ => Err(SHAPE.to_owned()),
    }
}

/// `{"brush": {"size": 0.1, "feather": 50, "flow": 100, "erase": false, "erase_held": true,
/// "nudge": ["size", -1]}}`: the brush the next stroke will be drawn with, by the field names
/// `mask.add-stroke` declares. There is no density, and naming one says so rather than being
/// ignored.
fn parse_brush(value: &Value) -> Result<BrushStep, String> {
    const SHAPE: &str = "mask brush takes size, feather, flow, erase, erase_held, limit_to_colour, \
         colour_refine or nudge: [FIELD, STEPS]";
    let object = value.as_object().ok_or(SHAPE)?;
    if object.is_empty() {
        return Err(SHAPE.to_owned());
    }
    let mut step = BrushStep::default();
    for (key, value) in object {
        let number = || -> Result<f64, String> {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| format!("mask brush {key} takes a number"))
        };
        let flag = || -> Result<bool, String> {
            value
                .as_bool()
                .ok_or_else(|| format!("mask brush {key} takes a flag"))
        };
        match key.as_str() {
            "size" => step.size = Some(number()?),
            "feather" => step.feather = Some(number()?),
            "flow" => step.flow = Some(number()?),
            "erase" => step.erase = Some(flag()?),
            "erase_held" => step.erase_held = Some(flag()?),
            "limit_to_colour" => step.limit_to_colour = Some(flag()?),
            "colour_refine" => step.colour_refine = Some(number()?),
            "nudge" => {
                let pair = value
                    .as_array()
                    .ok_or("mask brush nudge takes [FIELD, STEPS]")?;
                match (
                    pair.first().and_then(Value::as_str),
                    pair.get(1).and_then(Value::as_f64),
                ) {
                    (Some(name), Some(steps)) if pair.len() == 2 => {
                        step.nudge = Some((name.to_owned(), steps));
                    }
                    _ => return Err("mask brush nudge takes [FIELD, STEPS]".to_owned()),
                }
            }
            // Density is deliberately not delivered: its meaning depends on a build-up model along
            // one stroke, which would make coverage depend on stamp spacing and so on resolution.
            "density" => {
                return Err(
                    "there is no brush density: it would make coverage depend on stamp spacing \
                     and therefore on resolution. Flow is the per-stroke amount"
                        .to_owned(),
                );
            }
            other => return Err(format!("unknown mask brush field {other}")),
        }
    }
    Ok(step)
}

/// `{"stroke": {"points": [[x, y], …], "release": true}}`: one painted stroke, in normalized content
/// coordinates, released unless the step leaves the pointer down.
fn parse_stroke(value: &Value) -> Result<MaskStep, String> {
    const SHAPE: &str = "mask stroke takes {\"points\": [[x, y], …]} and an optional release flag";
    let object = value.as_object().ok_or(SHAPE)?;
    let mut release = true;
    let mut points: Option<Vec<[f64; 2]>> = None;
    for (key, value) in object {
        match key.as_str() {
            "release" => release = value.as_bool().ok_or("mask stroke release takes a flag")?,
            "points" => {
                let listed = value.as_array().ok_or(SHAPE)?;
                points = Some(
                    listed
                        .iter()
                        .map(|point| content_point(Some(point), "mask stroke point"))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            other => return Err(format!("unknown mask stroke field {other}")),
        }
    }
    let points = points
        .filter(|path| !path.is_empty())
        .ok_or("mask stroke needs at least one point")?;
    Ok(MaskStep::Stroke { points, release })
}

/// `{"row": {"component": REF, "mode": "subtract" | "invert": true | "index": N | "delete": true}}`:
/// one row control, by the row it belongs to and the one value it changes.
fn parse_mask_row(value: &Value) -> Result<MaskStep, String> {
    const SHAPE: &str =
        "mask row takes a component and one of mode, invert, index, delete or delete_stroke";
    let object = value.as_object().ok_or(SHAPE)?;
    let component = Reference::parse(
        object
            .get("component")
            .ok_or("mask row needs the component it edits")?,
        "component",
    )?;
    let mut edit = None;
    for (key, value) in object {
        let chosen = match key.as_str() {
            "component" => continue,
            "mode" => RowStep::Mode(
                value
                    .as_str()
                    .ok_or("mask row mode takes a declared mode")?
                    .to_owned(),
            ),
            "invert" => RowStep::Invert(value.as_bool().ok_or("mask row invert takes a flag")?),
            "index" => {
                RowStep::Move(value.as_u64().ok_or("mask row index takes a position")? as usize)
            }
            "delete" => {
                if value.as_bool() != Some(true) {
                    return Err("mask row delete takes true".to_owned());
                }
                RowStep::Delete
            }
            "delete_stroke" => RowStep::DeleteStroke(Reference::parse(value, "stroke")?),
            other => return Err(format!("unknown mask row field {other}")),
        };
        if edit.replace(chosen).is_some() {
            return Err(SHAPE.to_owned());
        }
    }
    Ok(MaskStep::Row {
        component,
        edit: edit.ok_or(SHAPE)?,
    })
}

/// Whether one component kind is **typed**: created straight away from the defaults its own geometry
/// declares, rather than drawn as a gesture. Read from the host's declarations, which is the same
/// question the panel's own button asks, so the two can never disagree about which route a kind takes.
fn mask_kind_is_typed(kind: &str) -> bool {
    !crate::mask_draft::drawable(kind)
        && lightwell_core::mask::component_geometry_is_defaulted(kind)
}

/// One normalized content position, in the stored range the mask study froze.
fn content_point(value: Option<&Value>, field: &str) -> Result<[f64; 2], String> {
    let pair = value
        .and_then(Value::as_array)
        .filter(|pair| pair.len() == 2)
        .ok_or_else(|| format!("{field} takes [x, y]"))?;
    let mut point = [0.0; 2];
    for (slot, value) in point.iter_mut().zip(pair) {
        *slot = value
            .as_f64()
            .filter(|value| value.is_finite() && (-1.0..=2.0).contains(value))
            .ok_or_else(|| format!("{field} takes finite normalized positions from -1 to 2"))?;
    }
    Ok(point)
}

fn finish(object: &Map<String, Value>, step: &str) -> Result<SliderEnd, String> {
    match object.get("finish").and_then(Value::as_str) {
        None if !object.contains_key("finish") => Ok(SliderEnd::Open),
        Some("open") => Ok(SliderEnd::Open),
        Some("release") => Ok(SliderEnd::Release),
        Some("cancel") => Ok(SliderEnd::Cancel),
        _ => Err(format!("{step} finish is open, release or cancel")),
    }
}

fn unit_number(value: &Value, field: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .ok_or_else(|| format!("{field} needs a finite fraction from 0 to 1"))
}

fn unit_fraction(value: &Value, field: &str) -> Result<f32, String> {
    unit_number(value, field).map(|value| value as f32)
}

fn point(value: &Value, field: &str) -> Result<[f32; 2], String> {
    let values = value
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| format!("{field} needs two fractions"))?;
    Ok([
        unit_fraction(&values[0], field)?,
        unit_fraction(&values[1], field)?,
    ])
}

fn required_index(object: &Map<String, Value>, field: &str, step: &str) -> Result<usize, String> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("{step} needs a non-negative whole {field}"))
}

fn parse_controls(value: &Value) -> Result<ControlsStep, String> {
    let object = value.as_object().ok_or("controls takes an object")?;
    known_fields(
        object,
        &[
            "action",
            "parameter",
            "gesture",
            "fractions",
            "finish",
            "value",
        ],
        "controls",
    )?;
    let action = required_text(object, "action", "controls")?;
    let parameter = required_text(object, "parameter", "controls")?;
    match object.get("gesture").and_then(Value::as_str) {
        Some("slider") => {
            if object.contains_key("value") {
                return Err("slider takes fractions, not value".into());
            }
            let fractions = object
                .get("fractions")
                .and_then(Value::as_array)
                .filter(|values| !values.is_empty())
                .ok_or("slider needs nonempty fractions")?
                .iter()
                .map(|value| unit_number(value, "slider fraction"))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ControlsStep::Slider {
                action,
                parameter,
                fractions,
                finish: finish(object, "slider")?,
            })
        }
        Some("discrete") => {
            if object.contains_key("fractions") || object.contains_key("finish") {
                return Err("discrete takes one value and commits once".into());
            }
            let value = object
                .get("value")
                .filter(|value| !value.is_null())
                .ok_or("discrete needs a typed value")?
                .clone();
            Ok(ControlsStep::Discrete {
                action,
                parameter,
                value,
            })
        }
        _ => Err("controls gesture is slider or discrete".into()),
    }
}

fn parse_picker(value: &Value) -> Result<PickerStep, String> {
    let object = value.as_object().ok_or("picker takes an object")?;
    known_fields(
        object,
        &["action", "parameter", "open", "hue", "plane", "finish"],
        "picker",
    )?;
    let open = match object.get("open") {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        _ => return Err("picker open takes true or false".into()),
    };
    let hue = object
        .get("hue")
        .map(|value| unit_fraction(value, "picker hue"))
        .transpose()?;
    let plane = object
        .get("plane")
        .map(|value| point(value, "picker plane"))
        .transpose()?;
    let finish = finish(object, "picker")?;
    if hue.is_none() && plane.is_none() && open.is_none() {
        return Err("picker needs open, hue or plane".into());
    }
    if hue.is_none() && plane.is_none() && finish != SliderEnd::Open {
        return Err("picker cannot release or cancel without a drag".into());
    }
    if open == Some(false) && (hue.is_some() || plane.is_some()) {
        return Err("picker cannot drag while closing".into());
    }
    Ok(PickerStep {
        action: required_text(object, "action", "picker")?,
        parameter: required_text(object, "parameter", "picker")?,
        open,
        hue,
        plane,
        finish,
    })
}

fn parse_curve(value: &Value) -> Result<CurveStep, String> {
    let object = value.as_object().ok_or("curve takes an object")?;
    known_fields(
        object,
        &[
            "action",
            "parameter",
            "event",
            "index",
            "points",
            "point",
            "finish",
        ],
        "curve",
    )?;
    let finish = finish(object, "curve")?;
    let event = match object.get("event").and_then(Value::as_str) {
        Some("move") => {
            if object.contains_key("point") {
                return Err("curve move takes points, not point".into());
            }
            let points = object
                .get("points")
                .and_then(Value::as_array)
                .filter(|points| !points.is_empty())
                .ok_or("curve move needs nonempty points")?
                .iter()
                .map(|value| point(value, "curve point"))
                .collect::<Result<Vec<_>, _>>()?;
            CurveStepEvent::Move {
                index: required_index(object, "index", "curve move")?,
                points,
            }
        }
        Some("add") => {
            if object.contains_key("index")
                || object.contains_key("points")
                || object.contains_key("finish")
            {
                return Err("curve add takes one point and commits once".into());
            }
            CurveStepEvent::Add(point(
                object.get("point").ok_or("curve add needs point")?,
                "curve point",
            )?)
        }
        Some("remove") => {
            if object.contains_key("point")
                || object.contains_key("points")
                || object.contains_key("finish")
            {
                return Err("curve remove takes one index and commits once".into());
            }
            CurveStepEvent::Remove(required_index(object, "index", "curve remove")?)
        }
        Some("channel") => {
            if object.contains_key("point")
                || object.contains_key("points")
                || object.contains_key("finish")
            {
                return Err("curve channel takes one index and changes no edit".into());
            }
            CurveStepEvent::Channel(required_index(object, "index", "curve channel")?)
        }
        _ => return Err("curve event is move, add, remove or channel".into()),
    };
    Ok(CurveStep {
        action: required_text(object, "action", "curve")?,
        parameter: required_text(object, "parameter", "curve")?,
        event,
        finish,
    })
}

fn parse_group(value: &Value) -> Result<GroupStep, String> {
    let object = value.as_object().ok_or("group takes an object")?;
    known_fields(object, &["module", "path", "expanded"], "group")?;
    let path = object
        .get("path")
        .and_then(Value::as_array)
        .filter(|path| !path.is_empty())
        .ok_or("group needs a nonempty path")?
        .iter()
        .map(|part| {
            part.as_u64()
                .and_then(|part| usize::try_from(part).ok())
                .ok_or_else(|| "group path takes non-negative whole indices".into())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let expanded = object
        .get("expanded")
        .and_then(Value::as_bool)
        .ok_or("group expanded takes true or false")?;
    Ok(GroupStep {
        module: required_text(object, "module", "group")?,
        path,
        expanded,
    })
}

fn parse_tab(value: &Value) -> Result<TabStep, String> {
    let object = value.as_object().ok_or("tab takes an object")?;
    known_fields(object, &["module", "index"], "tab")?;
    let index = object
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or("tab index takes a non-negative whole number")?;
    Ok(TabStep {
        module: required_text(object, "module", "tab")?,
        index,
    })
}

fn parse_section(value: &Value) -> Result<SectionStep, String> {
    let object = value.as_object().ok_or("section takes an object")?;
    known_fields(object, &["module", "expanded"], "section")?;
    let expanded = object
        .get("expanded")
        .and_then(Value::as_bool)
        .ok_or("section expanded takes true or false")?;
    Ok(SectionStep {
        module: required_text(object, "module", "section")?,
        expanded,
    })
}

fn parse_gallery(value: &Value) -> Result<Option<usize>, String> {
    let object = value.as_object().ok_or("gallery takes an object")?;
    known_fields(object, &["page"], "gallery")?;
    if object.get("page").is_some_and(Value::is_null) {
        return Ok(None);
    }
    let page = required_index(object, "page", "gallery")?;
    if crate::view::gallery_page_info(page).is_none() {
        return Err("gallery page is outside the component board".into());
    }
    Ok(Some(page))
}

#[cfg(test)]
mod control_script_tests {
    use super::*;

    #[test]
    fn first_slice_steps_parse_before_the_window_opens() {
        let script = json!([
            {"section":{"module":"lightwell.controls","expanded":true}},
            {"group":{"module":"lightwell.controls","path":[0],"expanded":false}},
            {"controls":{"action":"set-controls","parameter":"amount","gesture":"slider","fractions":[0.25,0.75],"finish":"release"}},
            {"controls":{"action":"set-controls","parameter":"enabled","gesture":"discrete","value":true}},
            {"picker":{"action":"set-controls","parameter":"rgb","open":true}},
            {"picker":{"action":"set-controls","parameter":"rgb","hue":0.125,"plane":[0.75,0.875],"finish":"cancel"}},
            {"curve":{"action":"set-controls","parameter":"master","event":"move","index":1,"points":[[0.5,0.375]],"finish":"open"}},
            {"curve":{"action":"set-controls","parameter":"master","event":"add","point":[0.25,0.25]}},
            {"curve":{"action":"set-controls","parameter":"master","event":"channel","index":1}},
            {"gallery":{"page":8}},
            {"tools_scroll":1.0}
        ]);
        let parsed = parse_script(&script.to_string()).unwrap();
        assert_eq!(parsed.len(), 11);
        assert!(matches!(
            parsed[2],
            Step::Controls(ControlsStep::Slider { .. })
        ));
        assert!(matches!(parsed[9], Step::Gallery(Some(8))));
    }

    #[test]
    fn first_slice_steps_reject_invalid_fractions_and_ambiguous_events() {
        for step in [
            json!({"controls":{"action":"set-controls","parameter":"amount","gesture":"slider","fractions":[1.1]}}),
            json!({"picker":{"action":"set-controls","parameter":"rgb","plane":[0.5,-0.1]}}),
            json!({"curve":{"action":"set-controls","parameter":"master","event":"add","point":[0.5,0.5],"finish":"release"}}),
            json!({"curve":{"action":"set-controls","parameter":"master","event":"move","index":1,"points":[]}}),
            json!({"gallery":{"page":-1}}),
        ] {
            assert!(parse_script(&json!([step]).to_string()).is_err());
        }
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
        &[
            "action",
            "parameter",
            "values",
            "release",
            "cancel",
            "interval_ms",
        ],
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
    let interval_ms = match object.get("interval_ms") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or("slider interval_ms is a positive integer")?,
        ),
    };
    Ok(SliderStep {
        action: required_text(object, "action", "slider")?,
        parameter: required_text(object, "parameter", "slider")?,
        values,
        end: match (release, cancel) {
            (true, _) => SliderEnd::Release,
            (_, true) => SliderEnd::Cancel,
            _ => SliderEnd::Open,
        },
        interval_ms,
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

/// One canvas pick's coordinates: pixels of the raster on screen, which is what the canvas itself
/// publishes. Mapping them to the content stage is the core's answer, never the script's.
fn parse_pick(value: &Value) -> Result<PickStep, String> {
    let object = value
        .as_object()
        .ok_or("pick takes an object with an x and a y")?;
    known_fields(object, &["x", "y"], "pick")?;
    let coordinate = |field: &str| -> Result<u32, String> {
        object
            .get(field)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("pick needs a non-negative whole {field}"))
    };
    Ok(PickStep {
        x: coordinate("x")?,
        y: coordinate("y")?,
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

fn selected_curve_channel(
    tools: &crate::state::tools::ToolsModel,
    action: &str,
    parameter: &str,
) -> Option<usize> {
    fn find(
        controls: &[crate::state::tools::ControlModel],
        action: &str,
        parameter: &str,
    ) -> Option<usize> {
        controls.iter().find_map(|control| match control {
            crate::state::tools::ControlModel::Group(group) => {
                find(&group.controls, action, parameter)
            }
            crate::state::tools::ControlModel::Curve(curve)
                if curve.action == action
                    && curve
                        .channels
                        .iter()
                        .any(|channel| channel.parameter == parameter) =>
            {
                Some(curve.selected_channel)
            }
            _ => None,
        })
    }
    tools
        .all()
        .find_map(|section| find(&section.controls, action, parameter))
}

/// `{"name": "...", "group": "..."}`: the exact name, and the group only to tell two apart.
fn parse_preset_pick(value: &Value, step: &str) -> Result<PresetPick, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{step} takes an object with a name and an optional group"))?;
    known_fields(object, &["name", "group"], step)?;
    Ok(PresetPick {
        name: required_text(object, "name", step)?,
        group: match object.get("group") {
            None | Some(Value::Null) => None,
            Some(_) => Some(required_text(object, "group", step)?),
        },
    })
}

/// `{"name": "...", "group": "...", "groups": ["<Module> · <Group>", ...], "submit": false}`:
/// `groups` lists exactly the checkboxes left checked, so every other one is unchecked, and
/// `submit`, true unless given, presses Create.
fn parse_preset_create(value: &Value) -> Result<PresetCreateStep, String> {
    let object = value
        .as_object()
        .ok_or("preset_create takes an object with a name, an optional group and groups")?;
    known_fields(
        object,
        &["name", "group", "groups", "submit"],
        "preset_create",
    )?;
    let groups = object
        .get("groups")
        .and_then(Value::as_array)
        .ok_or("preset_create needs groups: the labels of the checkboxes to leave checked")?
        .iter()
        .map(|label| {
            label
                .as_str()
                .filter(|label| !label.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "preset_create groups are checkbox labels".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PresetCreateStep {
        name: required_text(object, "name", "preset_create")?,
        group: match object.get("group") {
            None | Some(Value::Null) => None,
            Some(_) => Some(required_text(object, "group", "preset_create")?),
        },
        groups,
        submit: match object.get("submit") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(submit)) => *submit,
            Some(_) => return Err("preset_create submit takes true or false".into()),
        },
    })
}

/// `{"path": "fixtures/presets/develop.xmp"}`.
fn parse_preset_import(value: &Value) -> Result<Step, String> {
    let object = value
        .as_object()
        .ok_or("preset_import takes an object with a path")?;
    known_fields(object, &["path"], "preset_import")?;
    Ok(Step::PresetImport(required_text(
        object,
        "path",
        "preset_import",
    )?))
}

/// When a method takes no mutation envelope, whether it names the open asset; `None` for a method
/// that takes the envelope, or one the method table does not list, which keep it. Read from the
/// schema the method table publishes, so no method is named here.
pub(crate) fn envelope_free(method: &str) -> Option<bool> {
    let schema = lightwell_core::schemas(&lightwell_core::ModuleRegistry::builtin());
    let spec = schema["methods"].get(method)?;
    let names = |list: &Value| -> Vec<String> {
        match list {
            Value::Array(names) => names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect(),
            Value::Object(names) => names.keys().cloned().collect(),
            _ => Vec::new(),
        }
    };
    let required = names(&spec["required"]);
    if required.iter().any(|name| name == "mutation") {
        return None;
    }
    Some(
        required
            .iter()
            .chain(names(&spec["optional"]).iter())
            .any(|name| name == "asset_id"),
    )
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
        "angle_rail" => {
            let fractions: Vec<f64> = value
                .as_array()
                .ok_or("draft angle_rail takes rail fractions from 0 to 1")?
                .iter()
                .filter_map(|number| {
                    number
                        .as_f64()
                        .filter(|number| (0.0..=1.0).contains(number))
                })
                .collect();
            match value.as_array() {
                Some(values) if !values.is_empty() && fractions.len() == values.len() => {
                    Ok(DraftStep::AngleRail(fractions))
                }
                _ => Err("draft angle_rail takes one or more rail fractions from 0 to 1".into()),
            }
        }
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
        "workspace takes an object with any of state_panel, tools_panel, mode, thirds, clip_shadows, clip_highlights, mask_overlay or mask_overlay_colour",
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
            "mask_overlay" | "mask_overlay_colour" => {
                let word = value
                    .as_str()
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| format!("workspace {key} takes one of the declared words"))?
                    .to_owned();
                if key == "mask_overlay" {
                    step.mask_overlay = Some(word);
                } else {
                    step.mask_overlay_colour = Some(word);
                }
            }
            "mode" => {
                step.mode = Some(
                    value
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                        .ok_or("workspace mode takes a module id, \"pointer\" or \"mask\"")?
                        .to_owned(),
                );
            }
            other => return Err(format!("unknown workspace field {other}")),
        }
    }
    if step == WorkspaceStep::default() {
        return Err(
            "workspace needs at least one of state_panel, tools_panel, mode, thirds, clip_shadows, clip_highlights, mask_overlay or mask_overlay_colour"
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
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":[1],"interval_ms":0}}]"#,
                "positive integer",
            ),
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":[1],"interval_ms":-8}}]"#,
                "positive integer",
            ),
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":[1],"interval_ms":8.5}}]"#,
                "positive integer",
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

    /// Every mask verb parses from the shape a script writes and records itself back in that same
    /// shape, in all three spellings of a reference: a position, a name and an identity. One
    /// vocabulary drives both the panel's list edits and the canvas's own gestures, so a script may
    /// mix them.
    #[test]
    fn every_mask_verb_round_trips_its_script() {
        let script = r#"[{"mask":{"select":0}},
                {"mask":{"select":{"name":"Mask 1"}}},
                {"mask":{"select":"01JA00000000000000000000"}},
                {"mask":{"select_component":1}},
                {"mask":{"select_component":null}},
                {"mask":{"hover":{"name":"Linear 1"}}},
                {"mask":{"hover":null}},
                {"mask":{"edit_shape":0}},
                {"mask":{"mode":"subtract"}},
                {"mask":{"new":"linear"}},
                {"mask":{"add":"radial"}},
                {"mask":{"paint":"new-mask"}},
                {"mask":{"paint":"new-brush"}},
                {"mask":{"paint":{"component":{"name":"Brush 1"}}}},
                {"mask":{"brush":{"size":0.08,"feather":50.0,"flow":100.0}}},
                {"mask":{"brush":{"erase":true,"erase_held":false}}},
                {"mask":{"brush":{"nudge":["size",-2.0]}}},
                {"mask":{"stroke":{"points":[[0.3,0.3],[0.4,0.35]],"release":true}}},
                {"mask":{"stroke":{"points":[[0.5,0.5]],"release":false}}},
                {"mask":{"sweep":{"from":[0.5,0.2],"to":[0.5,0.8]}}},
                {"mask":{"release":true}},
                {"mask":{"drag":{"handle":"feather","points":[[0.4,0.4],[0.45,0.4]]}}},
                {"mask":{"apply":true}},
                {"mask":{"cancel":true}},
                {"mask":{"row":{"component":1,"mode":"intersect"}}},
                {"mask":{"row":{"component":{"name":"Radial 1"},"invert":true}}},
                {"mask":{"row":{"component":0,"index":2}}},
                {"mask":{"row":{"component":2,"delete":true}}},
                {"mask":{"row":{"component":0,"delete_stroke":1}}},
                {"mask":{"row":{"component":{"name":"Brush 1"},"delete_stroke":{"name":"Stroke 2"}}}},
                {"mask":{"row":{"component":0,"delete_stroke":"01JA0000000000000000000B"}}}]"#;
        let steps = parse_script(script).expect("a valid mask script");
        let written: Vec<Value> = serde_json::from_str(script).expect("the script is JSON");
        assert_eq!(steps.len(), written.len());
        for (step, written) in steps.iter().zip(&written) {
            assert_eq!(&step.record(), written);
        }
        assert_eq!(steps[0], Step::Mask(MaskStep::Select(Reference::Index(0))));
        assert_eq!(
            steps[1],
            Step::Mask(MaskStep::Select(Reference::Name("Mask 1".into())))
        );
        assert_eq!(
            steps[2],
            Step::Mask(MaskStep::Select(Reference::Id(
                "01JA00000000000000000000".into()
            )))
        );
        assert_eq!(steps[4], Step::Mask(MaskStep::SelectComponent(None)));
        assert_eq!(steps[6], Step::Mask(MaskStep::Hover(None)));
        assert_eq!(steps[11], Step::Mask(MaskStep::Paint(PaintStep::NewMask)));
        assert_eq!(steps[12], Step::Mask(MaskStep::Paint(PaintStep::NewBrush)));
        assert_eq!(
            steps[13],
            Step::Mask(MaskStep::Paint(PaintStep::Component(Reference::Name(
                "Brush 1".into()
            ))))
        );
        assert_eq!(
            steps[14],
            Step::Mask(MaskStep::Brush(BrushStep {
                size: Some(0.08),
                feather: Some(50.0),
                flow: Some(100.0),
                ..BrushStep::default()
            }))
        );
        assert_eq!(
            steps[16],
            Step::Mask(MaskStep::Brush(BrushStep {
                nudge: Some(("size".into(), -2.0)),
                ..BrushStep::default()
            }))
        );
        assert_eq!(
            steps[17],
            Step::Mask(MaskStep::Stroke {
                points: vec![[0.3, 0.3], [0.4, 0.35]],
                release: true
            })
        );
        assert_eq!(
            steps[18],
            Step::Mask(MaskStep::Stroke {
                points: vec![[0.5, 0.5]],
                release: false
            })
        );
        assert_eq!(
            steps[19],
            Step::Mask(MaskStep::Sweep {
                from: [0.5, 0.2],
                to: [0.5, 0.8]
            })
        );
        assert_eq!(
            steps[24],
            Step::Mask(MaskStep::Row {
                component: Reference::Index(1),
                edit: RowStep::Mode("intersect".into())
            })
        );
        // A stroke is named the same three ways every other object in a script is.
        assert_eq!(
            steps[28],
            Step::Mask(MaskStep::Row {
                component: Reference::Index(0),
                edit: RowStep::DeleteStroke(Reference::Index(1))
            })
        );
        assert_eq!(
            steps[29],
            Step::Mask(MaskStep::Row {
                component: Reference::Name("Brush 1".into()),
                edit: RowStep::DeleteStroke(Reference::Name("Stroke 2".into()))
            })
        );
        assert_eq!(
            steps[30],
            Step::Mask(MaskStep::Row {
                component: Reference::Index(0),
                edit: RowStep::DeleteStroke(Reference::Id("01JA0000000000000000000B".into()))
            })
        );

        for (script, expected) in [
            (r#"[{"mask":{"nowhere":true}}]"#, "unknown mask step"),
            (r#"[{"mask":{"select":0,"add":"linear"}}]"#, "exactly one"),
            (r#"[{"mask":{"select":true}}]"#, "position in the list"),
            (r#"[{"mask":{"select":{"id":"x"}}}]"#, "expected name"),
            (r#"[{"mask":{"select":{"name":" "}}}]"#, "non-empty string"),
            (r#"[{"mask":{"edit_shape":null}}]"#, "takes an identity"),
            (r#"[{"mask":{"apply":false}}]"#, "takes true"),
            (r#"[{"mask":{"new":""}}]"#, "component kind"),
            (r#"[{"mask":{"sweep":{"from":[0.5,0.2]}}}]"#, "takes [x, y]"),
            (
                r#"[{"mask":{"sweep":{"from":[0,0],"to":[0,9]}}}]"#,
                "-1 to 2",
            ),
            (
                r#"[{"mask":{"drag":{"handle":"corner","points":[[0.1,0.1]]}}}]"#,
                "unknown mask handle",
            ),
            (
                r#"[{"mask":{"drag":{"handle":"start","points":[]}}}]"#,
                "at least one point",
            ),
            (
                r#"[{"mask":{"row":{"mode":"add"}}}]"#,
                "the component it edits",
            ),
            (r#"[{"mask":{"row":{"component":0}}}]"#, "one of mode"),
            (
                r#"[{"mask":{"row":{"component":0,"mode":"add","invert":true}}}]"#,
                "one of mode",
            ),
            (r#"[{"mask":{"paint":"new-shape"}}]"#, "new-mask, new-brush"),
            (
                r#"[{"mask":{"paint":{"kind":"brush"}}}]"#,
                "unknown mask paint field kind",
            ),
            (
                r#"[{"mask":{"paint":{"component":true}}}]"#,
                "takes an identity",
            ),
            (r#"[{"mask":{"brush":{"size":"big"}}}]"#, "takes a number"),
            (r#"[{"mask":{"brush":{"erase":1}}}]"#, "takes a flag"),
            (
                r#"[{"mask":{"brush":{"nudge":["size"]}}}]"#,
                "[FIELD, STEPS]",
            ),
            (
                r#"[{"mask":{"brush":{"opacity":1}}}]"#,
                "unknown mask brush field",
            ),
            // Density is refused by name rather than ignored, because it is deliberately absent.
            (r#"[{"mask":{"brush":{"density":50}}}]"#, "no brush density"),
            (
                r#"[{"mask":{"stroke":{"points":[]}}}]"#,
                "at least one point",
            ),
            (r#"[{"mask":{"stroke":{"points":[[0.5,9.0]]}}}]"#, "-1 to 2"),
            (
                r#"[{"mask":{"stroke":{"points":[[0.5,0.5]],"speed":2}}}]"#,
                "unknown mask stroke field",
            ),
            (
                r#"[{"mask":{"row":{"component":0,"delete_stroke":true}}}]"#,
                "takes an identity",
            ),
        ] {
            let error = parse_script(script).expect_err(script);
            assert!(error.contains(expected), "{script}: {error}");
        }
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
                interval_ms: None,
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

    /// A slider step's `interval_ms` parses into the paced step, records itself back beside the
    /// fields the unpaced step already writes, and is refused when it is not a positive integer.
    #[test]
    fn slider_interval_ms_paces_the_step_and_is_recorded_when_present() {
        let steps = parse_script(
            r#"[{"slider":{"action":"set-basic","parameter":"exposure","values":[0.1,0.2],"interval_ms":8,"release":true}}]"#,
        )
        .expect("a valid script");
        assert_eq!(
            steps[0],
            Step::Slider(SliderStep {
                action: "set-basic".into(),
                parameter: "exposure".into(),
                values: vec![0.1, 0.2],
                end: SliderEnd::Release,
                interval_ms: Some(8),
            })
        );
        assert_eq!(
            steps[0].record(),
            json!({"slider":{"action":"set-basic","parameter":"exposure","values":[0.1,0.2],"release":true,"cancel":false,"interval_ms":8}})
        );
    }

    /// A paced step sends nothing when it starts: its values wait in `paced_slider` for the timer
    /// that is gated on them, and each tick sends exactly one, in order, recording it as its own
    /// event and leaving the field showing the value it just sent. The last tick ends the gesture
    /// the way the step said to and clears `paced_slider`, which is also what stops the timer.
    #[test]
    fn a_paced_slider_step_sends_one_value_per_tick() {
        let (mut editor, catalog, _, _) = crate::app::testing::opened(Vec::new(), 4);
        let _ = editor.update(Message::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        )));
        editor.evidence = Some(Evidence {
            dir: std::env::temp_dir().join("lightwell-paced-slider-test"),
            queue: VecDeque::new(),
            opens: 1,
            script: parse_script(
                r#"[{"slider":{"action":"set-basic","parameter":"exposure","values":[0.1,0.2,0.3],"interval_ms":8,"release":true}}]"#,
            )
            .expect("a valid script"),
            step: 0,
            awaiting: None,
            current: None,
            steps: Vec::new(),
            frames: Vec::new(),
            capture_pending: false,
            saving: false,
            had_errors: false,
            paced_slider: None,
            tools_scroll: None,
        });
        editor.activity.requested = 1;

        let _ = editor.next_step();
        // Nothing is sent yet: the step only queued its values for the timer, so the field still
        // shows the neutral default rather than any of them.
        assert_eq!(editor.fields.get("set-basic", "exposure"), Some("0.00"));
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((3, 0)),
            "the step queues every value for its own timer to send"
        );

        let _ = editor.update(Message::PacedSliderTick);
        assert_eq!(
            editor.fields.get("set-basic", "exposure"),
            Some("0.10"),
            "one tick sends the first value"
        );
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((2, 1))
        );
        assert!(
            evidence(&editor).awaiting.is_none(),
            "the step has not settled while values remain"
        );

        let _ = editor.update(Message::PacedSliderTick);
        assert_eq!(editor.fields.get("set-basic", "exposure"), Some("0.20"));
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((1, 2))
        );

        let _ = editor.update(Message::PacedSliderTick);
        assert_eq!(
            editor.fields.get("set-basic", "exposure"),
            Some("0.30"),
            "the last tick sends the last value"
        );
        assert!(
            evidence(&editor).paced_slider.is_none(),
            "the last tick clears the paced state, which also stops its timer"
        );
        assert_eq!(
            evidence(&editor).awaiting,
            Some(Settle::Preview),
            "a released step ends exactly as the unpaced step does"
        );

        // A tick with nothing left to send is harmless.
        let _ = editor.update(Message::PacedSliderTick);
        finish(editor, catalog);
    }

    /// A pick step carries the two rendered coordinates a click publishes and nothing else: which
    /// content pixel they name, and what picking it does, belong to the core and to the mode.
    #[test]
    fn a_pick_step_round_trips_its_rendered_coordinates() {
        let steps = parse_script(r#"[{"pick":{"x":120,"y":80}}]"#).expect("a valid script");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0], Step::Pick(PickStep { x: 120, y: 80 }));
        assert_eq!(steps[0].record(), json!({"pick":{"x":120,"y":80}}));
        for refused in [
            r#"[{"pick":{"x":120}}]"#,
            r#"[{"pick":{"x":120,"y":-2}}]"#,
            r#"[{"pick":{"x":120,"y":80,"mode":"lightwell.basic"}}]"#,
        ] {
            assert!(parse_script(refused).is_err(), "{refused} was accepted");
        }
    }

    /// A scripted pick runs in the mode that is on screen and is refused when none of them picks.
    #[test]
    fn a_scripted_pick_needs_a_canvas_mode_that_declares_one() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"pick":{"x":7,"y":9}}]"#);
        let _ = editor.update(Message::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        )));
        // The pointer mode declares no pick, so the step is recorded as refused, not silently
        // dropped, and its frame is still captured.
        let _ = editor.next_step();
        assert!(evidence(&editor).had_errors);
        assert!(evidence(&editor).capture_pending);
        assert!(
            editor.status.contains("declares no pick"),
            "{}",
            editor.status
        );
        crate::app::testing::finish(editor, catalog);
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

    #[test]
    fn preset_steps_parse_strictly_and_record_what_was_written() {
        let script = json!([
            {"preset":{"name":"Soft film","group":"Synthetic"}},
            {"preset":{"name":"Soft Film"}},
            {"preset_create":{"name":"Tone only","group":"Looks","groups":["Basic \u{00b7} Tone"]}},
            {"preset_create":{"name":"Tone only","groups":[],"submit":false}},
            {"preset_delete":{"name":"Tone only"}},
            {"preset_import":{"path":"fixtures/presets/develop.xmp"}},
            {"api":{"method":"preset.list"}}
        ]);
        let steps = parse_script(&script.to_string()).expect("a valid script");
        assert_eq!(
            steps[0],
            Step::Preset(PresetPick {
                name: "Soft film".into(),
                group: Some("Synthetic".into())
            })
        );
        assert_eq!(
            steps[3],
            Step::PresetCreate(PresetCreateStep {
                name: "Tone only".into(),
                group: None,
                groups: Vec::new(),
                submit: false
            })
        );
        assert_eq!(
            steps[5],
            Step::PresetImport("fixtures/presets/develop.xmp".into())
        );
        for (step, written) in steps.iter().zip(script.as_array().unwrap()).take(6) {
            assert_eq!(&step.record(), written, "a record is the step as written");
        }
        assert_eq!(
            steps[6],
            Step::Api {
                method: "preset.list".into(),
                params: Map::new()
            }
        );
        for (script, expected) in [
            (r#"[{"preset":{}}]"#, "preset needs a name"),
            (
                r#"[{"preset":{"name":"x","at":1}}]"#,
                "unknown preset field",
            ),
            (
                r#"[{"preset":{"name":"x","group":""}}]"#,
                "preset needs a group",
            ),
            (
                r#"[{"preset_delete":{"group":"g"}}]"#,
                "preset_delete needs a name",
            ),
            (r#"[{"preset_create":{"name":"x"}}]"#, "needs groups"),
            (
                r#"[{"preset_create":{"name":"x","groups":[1]}}]"#,
                "checkbox labels",
            ),
            (
                r#"[{"preset_create":{"name":"x","groups":[],"submit":"no"}}]"#,
                "submit takes true or false",
            ),
            (r#"[{"preset_import":{}}]"#, "preset_import needs a path"),
            (
                r#"[{"preset_import":"x.xmp"}]"#,
                "takes an object with a path",
            ),
        ] {
            let error = parse_script(script).expect_err(script);
            assert!(error.contains(expected), "{script}: {error}");
        }
    }

    #[test]
    fn a_host_method_is_sent_as_written_and_an_edit_keeps_its_envelope() {
        // The method table's own schema decides: a method that takes the mutation envelope keeps
        // it, and any other goes as written, with the asset only where it names one.
        assert_eq!(envelope_free("preset.list"), Some(false));
        assert_eq!(envelope_free("session.state"), Some(false));
        assert_eq!(envelope_free("preset.capture"), Some(true));
        assert_eq!(envelope_free("history.undo"), None);
        assert_eq!(envelope_free("edit.apply-preset"), None);
        assert_eq!(envelope_free("no.such-method"), None);

        let (mut editor, catalog, _, _) = scripted(r#"[{"api":{"method":"preset.list"}}]"#);
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Host));
        assert!(!editor.busy, "a host call is no edit");
        assert_eq!(
            editor.activity.requested, requested,
            "and waits for no render"
        );
        let listing = vec![crate::app::testing::listed("Warm", "User presets", None)];
        editor.host_answered(Ok(HostAnswer {
            method: "preset.list".into(),
            result: json!({"presets":[]}),
            presets: Some(listing.clone()),
            sequence: 9,
        }));
        assert!(evidence(&editor).capture_pending);
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"));
        assert_eq!(record["result"], json!({"presets":[]}));
        assert_eq!(editor.presets.presets, Some(listing));
        finish(editor, catalog);
    }

    /// A scripted editor with every built-in discovered and this library listed.
    fn with_library(steps: &str, presets: Vec<lightwell_core::PresetSummary>) -> (Editor, PathBuf) {
        let (mut editor, catalog, _, _) = scripted(steps);
        let _ = editor.update(Message::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        )));
        let _ = editor.update(Message::Preset(PresetMessage::Listed(Ok((presets, 1)))));
        (editor, catalog)
    }

    #[test]
    fn a_preset_step_names_exactly_one_row_or_fails() {
        use crate::app::testing::listed;
        let a = listed("Warm", "A", None);
        let b = listed("Warm", "B", None);
        let (mut editor, catalog) = with_library(
            r#"[{"preset":{"name":"Warm"}},{"preset":{"name":"warm","group":"B"}},
                {"preset":{"name":"Warm","group":"B"}}]"#,
            vec![a, b.clone()],
        );
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert_eq!(
            record["reason"],
            json!("2 presets are named Warm; name its group")
        );
        // Names match exactly: case is part of a preset's name.
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["reason"], json!("no preset is named warm in B"));
        // The group tells them apart, and the click is the ordinary action path.
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"), "{record}");
        assert_eq!(record["preset_id"], json!(b.id.as_str()));
        assert!(editor.busy, "{}", editor.status);
        assert_eq!(editor.activity.requested, requested + 1);
        finish(editor, catalog);
    }

    #[test]
    fn preset_create_delete_and_import_steps_drive_the_sections_own_messages() {
        use crate::app::testing::listed;
        let warm = listed("Warm", "User presets", None);
        let (mut editor, catalog) = with_library(
            r#"[{"preset_create":{"name":"Tone only","groups":["Basic · Tone"],"submit":false}},
                {"preset_create":{"name":"Tone only","groups":["Basic · Tone"]}},
                {"preset_create":{"name":"Other","groups":["Nowhere · Group"]}}]"#,
            vec![warm.clone()],
        );
        // Filled and left open: the frame shows the form.
        let _ = editor.next_step();
        assert!(evidence(&editor).capture_pending);
        assert!(editor.preset_form.open);
        assert_eq!(editor.preset_form.name, "Tone only");
        let checked: Vec<_> = editor
            .preset_form
            .checked
            .iter()
            .filter(|(_, on)| **on)
            .map(|(label, _)| label.as_str())
            .collect();
        assert_eq!(checked, ["Basic \u{00b7} Tone"]);
        assert!(!editor.presets.pending);
        // Submitted: Create runs and the step waits for the library's answer.
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.pending, "{}", editor.status);
        // A label no group has fails the step before anything is sent.
        editor.presets.pending = false;
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(!editor.presets.pending);
        finish(editor, catalog);

        let (mut editor, catalog) = with_library(
            r#"[{"preset_delete":{"name":"Warm"}},{"preset_import":{"path":"missing.xmp"}}]"#,
            vec![warm.clone()],
        );
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.pending && editor.menu.is_none());
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["preset_id"], json!(warm.id.as_str()));
        editor.presets.pending = false;
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.pending, "the import task was started");
        // Its refusal is recorded on the step and captured.
        let _ = editor.update(Message::Preset(PresetMessage::Imported(Err(
            "read-error: cannot read missing.xmp".into(),
        ))));
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(evidence(&editor).capture_pending && evidence(&editor).had_errors);
        finish(editor, catalog);
    }
}
