//! The evidence script: the steps an evidence run of the desktop performs after its opens, one
//! captured frame per step.
//!
//! One set of serde types serves both ends. The desktop parses a script with [`parse`] before its
//! window opens and records each step it ran as [`Step::kept`] writes it; xtask builds each
//! scenario's script from the same types and writes it with [`write`]. A step is spelled one way
//! on the wire, and a script xtask can write is one the desktop reads back as the same value.
//!
//! A script is a JSON array of steps. A step is an object with exactly one key, its kind, whose
//! value is the step: `{"slider": {"action": "set-basic", "parameter": "exposure", "values": [1.0],
//! "release": true}}`. Parsing is strict: an unknown kind or field, a missing field or a value out
//! of its range fails the whole script, naming the step, so a malformed script fails the run
//! instead of producing partial evidence. Structure is checked by the serde derives and the rest
//! by [`Step::validate`].
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};

mod build;

/// The most steps one evidence run accepts, so a script cannot outlive the evidence deadline
/// unnoticed.
pub const MAX_SCRIPT_STEPS: usize = 64;

/// The longest one `wait` step may idle, so a script cannot spend its deadline doing nothing.
pub const MAX_WAIT_MS: u64 = 10_000;

/// The longest gap a scripted double-click may leave between its release and its second press.
/// Iced classifies two presses as a double-click only within 300 ms of each other, and the first
/// press's own hold comes out of that too.
pub const MAX_DOUBLE_CLICK_GAP_MS: u64 = 250;

/// What a secret's value is kept and recorded as.
pub const REDACTED: &str = "<redacted>";

/// Parse an evidence script. Every step is checked before any runs, and an error names the step by
/// its one-based position and its kind.
pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("evidence script is not JSON: {error}"))?;
    let Value::Array(steps) = value else {
        return Err("an evidence script is a JSON array of steps".into());
    };
    if steps.len() > MAX_SCRIPT_STEPS {
        return Err(format!(
            "at most {MAX_SCRIPT_STEPS} evidence script steps are supported per run"
        ));
    }
    steps
        .into_iter()
        .enumerate()
        .map(|(index, step)| {
            let kind = match &step {
                Value::Object(object) if object.len() == 1 => object.keys().next().cloned(),
                _ => None,
            };
            Step::from_value(step).map_err(|error| match kind {
                Some(kind) => format!("evidence script step {} ({kind}): {error}", index + 1),
                None => format!("evidence script step {}: {error}", index + 1),
            })
        })
        .collect()
}

/// A script as it is written to disk: the JSON array [`parse`] reads back as `steps`.
pub fn write(steps: &[Step]) -> Value {
    Value::Array(steps.iter().map(Step::to_value).collect())
}

/// One step of an evidence script. Steps run in order after the last `--open` outcome, each
/// followed by exactly one captured frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// One owner request. The desktop fills `asset_id` and the mutation envelope itself, so a script
    /// never carries a revision or a request id.
    Api {
        method: String,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        params: Map<String, Value>,
    },
    Draft(DraftStep),
    /// One slider gesture on a generated control: the exact messages a drag sends.
    Slider(SliderStep),
    /// A double-click on a slider's rail: a committed jump, then the reset, `gap_ms` apart.
    DoubleClick(DoubleClickStep),
    /// A first-slice generated slider (fraction) or discrete control gesture.
    Controls(ControlsStep),
    Picker(PickerStep),
    Curve(CurveStep),
    Group(GroupStep),
    /// The tab a tabbed section shows, as its tab row selects it.
    Tab(TabStep),
    Section(SectionStep),
    /// The gallery page shown instead of the workspace, or the workspace again for `null`.
    Gallery {
        #[serde(deserialize_with = "Option::deserialize")]
        page: Option<usize>,
    },
    /// The tools panel scrolled to this fraction of its range.
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
    /// Click one library preset's row.
    Preset(PresetPick),
    /// Open the create form, type its name and group, set every checkbox and press Create.
    PresetCreate(PresetCreateStep),
    /// Delete one library preset through its row's context menu.
    PresetDelete(PresetPick),
    /// Import one file through the section's own import task, bypassing only the native dialog.
    /// The path is as the script wrote it, relative to the editor's working directory.
    PresetImport {
        path: String,
    },
    /// Open or close the state panel's Performance section, as its heading does.
    Performance {
        expanded: bool,
    },
    /// Ask nothing of the editor for at least this many milliseconds, then capture. The evidence
    /// tick keeps rebuilding the view meanwhile, as the editor's own event sync does while a
    /// photograph is open, so the frame shows what idling did to the screen.
    Wait {
        ms: u64,
    },
    /// Scroll the percent-zoom surface to a fraction of its scrollable range on each axis, as a
    /// pan does, and capture once the offset it reports has reached the owner's session.
    Pan {
        x: f32,
        y: f32,
    },
    /// One gesture on a module's capability section, task control or consent notice.
    Capability(CapabilityStep),
    /// One Masks-panel view or mask-canvas gesture.
    Mask(MaskStep),
}

impl Step {
    /// One step from its JSON value, checked as [`parse`] checks it.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let step: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        step.validate()?;
        Ok(step)
    }

    /// The step as a script writes it.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("an evidence step always serializes")
    }

    /// The step as it is kept beside the evidence and recorded beside its frame: a secret's value
    /// replaced by [`REDACTED`], everything else as written.
    pub fn kept(&self) -> Value {
        let mut step = self.clone();
        if let Self::Capability(CapabilityStep {
            action: CapabilityAction::Secret { value, .. },
            ..
        }) = &mut step
        {
            *value = Secret::new(REDACTED.into());
        }
        step.to_value()
    }

    /// The checks the types alone do not make: ranges, non-empty names, and the combinations a
    /// step's fields may take.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Api { method, params } => {
                text(method, "api method")?;
                for reserved in ["asset_id", "mutation"] {
                    if params.contains_key(reserved) {
                        return Err(format!(
                            "the desktop fills {reserved} itself; a script may not set it"
                        ));
                    }
                }
                Ok(())
            }
            Self::Draft(step) => step.validate(),
            Self::Slider(step) => step.validate(),
            Self::DoubleClick(step) => step.validate(),
            Self::Controls(step) => step.validate(),
            Self::Picker(step) => step.validate(),
            Self::Curve(step) => step.validate(),
            Self::Group(step) => {
                text(&step.module, "group module")?;
                if step.path.is_empty() {
                    return Err("group needs a nonempty path".into());
                }
                Ok(())
            }
            Self::Tab(step) => text(&step.module, "tab module"),
            Self::Section(step) => text(&step.module, "section module"),
            Self::Gallery { .. } | Self::Hover { .. } | Self::Pick(_) | Self::SliderDraft(_) => {
                Ok(())
            }
            Self::ToolsScroll(fraction) => unit(*fraction, "tools_scroll"),
            Self::Field(step) => {
                text(&step.action, "field action")?;
                text(&step.parameter, "field parameter")
            }
            Self::Reset(step) => {
                text(&step.module, "reset module")?;
                optional_text(step.group.as_deref(), "reset group")
            }
            Self::View(step) => step.validate(),
            Self::Workspace(step) => step.validate(),
            Self::Preview(_) | Self::Palette(_) | Self::Performance { .. } => Ok(()),
            Self::Preset(pick) | Self::PresetDelete(pick) => pick.validate(),
            Self::PresetCreate(step) => step.validate(),
            Self::PresetImport { path } => text(path, "preset_import path"),
            Self::Wait { ms } => {
                if (1..=MAX_WAIT_MS).contains(ms) {
                    Ok(())
                } else {
                    Err(format!("wait ms takes an integer from 1 to {MAX_WAIT_MS}"))
                }
            }
            Self::Pan { x, y } => {
                unit(f64::from(*x), "pan x")?;
                unit(f64::from(*y), "pan y")
            }
            Self::Capability(step) => step.validate(),
            Self::Mask(step) => step.validate(),
        }
    }
}

/// A name a step needs, which may not be blank.
fn text(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} takes a non-empty string"))
    } else {
        Ok(())
    }
}

fn optional_text(value: Option<&str>, field: &str) -> Result<(), String> {
    value.map_or(Ok(()), |value| text(value, field))
}

/// A fraction of a range, from 0 to 1.
fn unit(value: f64, field: &str) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{field} needs a finite fraction from 0 to 1"))
    }
}

fn point(point: [f32; 2], field: &str) -> Result<(), String> {
    unit(f64::from(point[0]), field)?;
    unit(f64::from(point[1]), field)
}

/// One normalized content position, in the stored range the mask study froze.
fn content_point(point: [f64; 2], field: &str) -> Result<(), String> {
    if point
        .iter()
        .all(|value| value.is_finite() && (-1.0..=2.0).contains(value))
    {
        Ok(())
    } else {
        Err(format!(
            "{field} takes finite normalized positions from -1 to 2"
        ))
    }
}

fn positive(interval_ms: Option<u64>, field: &str) -> Result<(), String> {
    match interval_ms {
        Some(0) => Err(format!("{field} is a positive integer")),
        _ => Ok(()),
    }
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn yes() -> bool {
    true
}

/// A key whose only value is `true`, which is how a script asks for an action that takes nothing:
/// `{"apply": true}`. Any other value is refused rather than read as "don't".
fn only_true<'de, D: Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    if bool::deserialize(deserializer)? {
        Ok(())
    } else {
        Err(D::Error::custom("takes true"))
    }
}

fn write_true<S: Serializer>(serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(true)
}

/// `true`, as a field of a step whose other fields decide what it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct True;

impl Serialize for True {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        write_true(serializer)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        only_true(deserializer).map(|()| True)
    }
}

/// One crop-draft change, each mapped to the message the panel or the canvas would send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStep {
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Start,
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Reapply,
    Angle(f64),
    Nudge(f64),
    /// A drag on the angle's rail through these fractions of its range, then its release, exactly
    /// as the rail publishes them.
    AngleRail(Vec<f64>),
    /// A declared `aspect` option, by name; an undeclared one fails the step.
    Preset(String),
    /// A rectangle `[x, y, width, height]` in box pixels, applied as two corner gestures.
    Rect([f64; 4]),
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Swap,
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Lock,
    Option(bool),
    Guide(bool),
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Apply,
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Cancel,
}

impl DraftStep {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::AngleRail(fractions) => {
                if fractions.is_empty()
                    || fractions
                        .iter()
                        .any(|fraction| unit(*fraction, "").is_err())
                {
                    return Err(
                        "draft angle_rail takes one or more rail fractions from 0 to 1".into(),
                    );
                }
                Ok(())
            }
            Self::Preset(option) => text(option, "draft preset"),
            Self::Rect([_, _, width, height]) if *width <= 0.0 || *height <= 0.0 => Err(
                "draft rect takes four finite numbers with positive extents, in box pixels".into(),
            ),
            _ => Ok(()),
        }
    }
}

/// How a scripted gesture ends: released (committed), Escape (cancelled), or left open so the
/// frame shows the draft mid-gesture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SliderEnd {
    Release,
    Cancel,
    #[default]
    Open,
}

/// One slider gesture. Every value becomes one `SliderMoved` with a tick between them, exactly as
/// a pointer drag and the gated subscription produce them; the gesture then ends the way `end`
/// says, or stays open when it says nothing.
///
/// Without `interval_ms` every value is sent at once, as a fast drag would coalesce between ticks.
/// With it, one value is sent per tick of its own gated timer instead, which is how a wild,
/// undrained drag is scripted.
///
/// On the wire the end is two flags, `release` and `cancel`, both false for an open gesture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SliderWire", into = "SliderWire")]
pub struct SliderStep {
    pub action: String,
    pub parameter: String,
    pub values: Vec<f64>,
    pub end: SliderEnd,
    pub interval_ms: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SliderWire {
    action: String,
    parameter: String,
    values: Vec<f64>,
    #[serde(default)]
    release: bool,
    #[serde(default)]
    cancel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interval_ms: Option<u64>,
}

impl TryFrom<SliderWire> for SliderStep {
    type Error = String;

    fn try_from(wire: SliderWire) -> Result<Self, String> {
        let end = match (wire.release, wire.cancel) {
            (true, true) => {
                return Err("a slider step either releases or cancels, not both".into());
            }
            (true, false) => SliderEnd::Release,
            (false, true) => SliderEnd::Cancel,
            (false, false) => SliderEnd::Open,
        };
        Ok(Self {
            action: wire.action,
            parameter: wire.parameter,
            values: wire.values,
            end,
            interval_ms: wire.interval_ms,
        })
    }
}

impl From<SliderStep> for SliderWire {
    fn from(step: SliderStep) -> Self {
        Self {
            action: step.action,
            parameter: step.parameter,
            values: step.values,
            release: step.end == SliderEnd::Release,
            cancel: step.end == SliderEnd::Cancel,
            interval_ms: step.interval_ms,
        }
    }
}

impl SliderStep {
    fn validate(&self) -> Result<(), String> {
        text(&self.action, "slider action")?;
        text(&self.parameter, "slider parameter")?;
        if self.values.is_empty() {
            return Err("slider needs at least one value".into());
        }
        positive(self.interval_ms, "slider interval_ms")
    }
}

/// One double-click on a generated slider's rail, as the rail's wrapper and iced's slider turn it
/// into messages: the first press moves the value to `value`, which opens the control's gesture,
/// and its release commits it; `gap_ms` after that release, at most [`MAX_DOUBLE_CLICK_GAP_MS`],
/// the second press is the wrapper's reset of the field. The step never waits between the two
/// presses for anything but the gap, so the reset meets whatever the first press's commit is still
/// doing, exactly as a person's does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleClickStep {
    pub action: String,
    pub parameter: String,
    pub value: f64,
    pub gap_ms: u64,
}

impl DoubleClickStep {
    fn validate(&self) -> Result<(), String> {
        text(&self.action, "double_click action")?;
        text(&self.parameter, "double_click parameter")?;
        if self.gap_ms > MAX_DOUBLE_CLICK_GAP_MS {
            return Err(format!(
                "double_click gap_ms is an integer from 0 to {MAX_DOUBLE_CLICK_GAP_MS}"
            ));
        }
        Ok(())
    }
}

/// A first-slice generated control's gesture: a slider dragged through fractions of its range, or
/// a discrete control set to one typed value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "gesture", rename_all = "lowercase", deny_unknown_fields)]
pub enum ControlsStep {
    Slider {
        action: String,
        parameter: String,
        fractions: Vec<f64>,
        #[serde(default)]
        finish: SliderEnd,
    },
    Discrete {
        action: String,
        parameter: String,
        value: Value,
    },
}

impl ControlsStep {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Slider {
                action,
                parameter,
                fractions,
                ..
            } => {
                text(action, "controls action")?;
                text(parameter, "controls parameter")?;
                if fractions.is_empty() {
                    return Err("slider needs nonempty fractions".into());
                }
                fractions
                    .iter()
                    .try_for_each(|fraction| unit(*fraction, "slider fraction"))
            }
            Self::Discrete {
                action,
                parameter,
                value,
            } => {
                text(action, "controls action")?;
                text(parameter, "controls parameter")?;
                if value.is_null() {
                    return Err("discrete needs a typed value".into());
                }
                Ok(())
            }
        }
    }
}

/// A colour picker: opened or closed, and its hue rail and plane dragged to fractions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickerStep {
    pub action: String,
    pub parameter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<[f32; 2]>,
    #[serde(default)]
    pub finish: SliderEnd,
}

impl PickerStep {
    fn validate(&self) -> Result<(), String> {
        text(&self.action, "picker action")?;
        text(&self.parameter, "picker parameter")?;
        if let Some(hue) = self.hue {
            unit(f64::from(hue), "picker hue")?;
        }
        if let Some(plane) = self.plane {
            point(plane, "picker plane")?;
        }
        let drags = self.hue.is_some() || self.plane.is_some();
        if !drags && self.open.is_none() {
            return Err("picker needs open, hue or plane".into());
        }
        if !drags && self.finish != SliderEnd::Open {
            return Err("picker cannot release or cancel without a drag".into());
        }
        if self.open == Some(false) && drags {
            return Err("picker cannot drag while closing".into());
        }
        Ok(())
    }
}

/// One curve editor gesture: a point dragged, added or removed, or a channel selected.
///
/// On the wire the event is a field of the step, `"event": "move"`, beside the fields that event
/// takes; only a move is a drag, so only a move takes `finish`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CurveWire", into = "CurveWire")]
pub struct CurveStep {
    pub action: String,
    pub parameter: String,
    pub event: CurveStepEvent,
    pub finish: SliderEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CurveStepEvent {
    Move { index: usize, points: Vec<[f32; 2]> },
    Add([f32; 2]),
    Remove(usize),
    Channel(usize),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase", deny_unknown_fields)]
enum CurveWire {
    Move {
        action: String,
        parameter: String,
        index: usize,
        points: Vec<[f32; 2]>,
        #[serde(default)]
        finish: SliderEnd,
    },
    Add {
        action: String,
        parameter: String,
        point: [f32; 2],
    },
    Remove {
        action: String,
        parameter: String,
        index: usize,
    },
    Channel {
        action: String,
        parameter: String,
        index: usize,
    },
}

impl From<CurveWire> for CurveStep {
    fn from(wire: CurveWire) -> Self {
        let (action, parameter, event, finish) = match wire {
            CurveWire::Move {
                action,
                parameter,
                index,
                points,
                finish,
            } => (
                action,
                parameter,
                CurveStepEvent::Move { index, points },
                finish,
            ),
            CurveWire::Add {
                action,
                parameter,
                point,
            } => (
                action,
                parameter,
                CurveStepEvent::Add(point),
                SliderEnd::Open,
            ),
            CurveWire::Remove {
                action,
                parameter,
                index,
            } => (
                action,
                parameter,
                CurveStepEvent::Remove(index),
                SliderEnd::Open,
            ),
            CurveWire::Channel {
                action,
                parameter,
                index,
            } => (
                action,
                parameter,
                CurveStepEvent::Channel(index),
                SliderEnd::Open,
            ),
        };
        Self {
            action,
            parameter,
            event,
            finish,
        }
    }
}

impl From<CurveStep> for CurveWire {
    fn from(step: CurveStep) -> Self {
        let CurveStep {
            action,
            parameter,
            event,
            finish,
        } = step;
        match event {
            CurveStepEvent::Move { index, points } => Self::Move {
                action,
                parameter,
                index,
                points,
                finish,
            },
            CurveStepEvent::Add(point) => Self::Add {
                action,
                parameter,
                point,
            },
            CurveStepEvent::Remove(index) => Self::Remove {
                action,
                parameter,
                index,
            },
            CurveStepEvent::Channel(index) => Self::Channel {
                action,
                parameter,
                index,
            },
        }
    }
}

impl CurveStep {
    fn validate(&self) -> Result<(), String> {
        text(&self.action, "curve action")?;
        text(&self.parameter, "curve parameter")?;
        match &self.event {
            CurveStepEvent::Move { points, .. } => {
                if points.is_empty() {
                    return Err("curve move needs nonempty points".into());
                }
                points
                    .iter()
                    .try_for_each(|value| point(*value, "curve point"))
            }
            CurveStepEvent::Add(value) => point(*value, "curve point"),
            CurveStepEvent::Remove(_) | CurveStepEvent::Channel(_) => Ok(()),
        }
    }
}

/// A control group expanded or collapsed, by its position inside the module's controls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupStep {
    pub module: String,
    pub path: Vec<usize>,
    pub expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabStep {
    pub module: String,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionStep {
    pub module: String,
    pub expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldStep {
    pub action: String,
    pub parameter: String,
    pub text: String,
    /// Enter in the field, which commits that one field without a draft.
    #[serde(default)]
    pub submit: bool,
}

/// One canvas pick, in pixels of the raster on screen: the same coordinates the canvas publishes
/// when a pointer is pressed over the photograph. Mapping them to the content stage is the core's
/// answer, never the script's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickStep {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetStep {
    pub module: String,
    /// A control group's label; without one the module's own header reset runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SliderDraftStep {
    Discard,
    Reapply,
}

/// The zoom: `{"zoom": "fit"}` or `{"zoom": 100.0}`, a percentage from 10 to 1600.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ViewWire", into = "ViewWire")]
pub enum ViewStep {
    Fit,
    Percent(f32),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewWire {
    zoom: Zoom,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged, expecting = "\"fit\" or a percentage from 10 to 1600")]
enum Zoom {
    Fit(Fit),
    Percent(f32),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Fit {
    Fit,
}

impl TryFrom<ViewWire> for ViewStep {
    type Error = String;

    fn try_from(wire: ViewWire) -> Result<Self, String> {
        match wire.zoom {
            Zoom::Fit(Fit::Fit) => Ok(Self::Fit),
            Zoom::Percent(percent) if percent.is_finite() && (10.0..=1600.0).contains(&percent) => {
                Ok(Self::Percent(percent))
            }
            Zoom::Percent(_) => Err("view zoom is \"fit\" or a percentage from 10 to 1600".into()),
        }
    }
}

impl From<ViewStep> for ViewWire {
    fn from(step: ViewStep) -> Self {
        Self {
            zoom: match step {
                ViewStep::Fit => Zoom::Fit(Fit::Fit),
                ViewStep::Percent(percent) => Zoom::Percent(percent),
            },
        }
    }
}

impl ViewStep {
    fn validate(&self) -> Result<(), String> {
        // The range is checked as the step is read; a step built in code is checked here.
        ViewStep::try_from(ViewWire::from(*self)).map(|_| ())
    }
}

/// Any of the panels, mode or thirds; every field is optional, exactly as `workspace.set` takes
/// them. At least one field is required.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_panel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_panel: Option<bool>,
    /// A module id, `pointer` or `mask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thirds: Option<bool>,
    /// The two clipping overlays, per-client view state like every other field here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_shadows: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_highlights: Option<bool>,
    /// What the canvas draws of the selected mask, and in which tint: one of the declared words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_overlay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_overlay_colour: Option<String>,
}

impl WorkspaceStep {
    fn validate(&self) -> Result<(), String> {
        if *self == Self::default() {
            return Err(
                "workspace needs at least one of state_panel, tools_panel, mode, thirds, \
                 clip_shadows, clip_highlights, mask_overlay or mask_overlay_colour"
                    .into(),
            );
        }
        optional_text(self.mode.as_deref(), "workspace mode")?;
        optional_text(self.mask_overlay.as_deref(), "workspace mask_overlay")?;
        optional_text(
            self.mask_overlay_colour.as_deref(),
            "workspace mask_overlay_colour",
        )
    }
}

/// Select a loaded history entry by its sequence number, or return to the current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewStep {
    Sequence(u64),
    Current,
}

/// Open the command palette with this query, or open it, run the query and run its first match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaletteStep {
    Query(String),
    Run(String),
}

/// One library preset, by its exact name, and by its group when two groups hold that name. A step
/// that matches no row, or more than one, fails rather than guessing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetPick {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl PresetPick {
    fn validate(&self) -> Result<(), String> {
        text(&self.name, "preset name")?;
        optional_text(self.group.as_deref(), "preset group")
    }
}

/// The create form as a script fills it: the name, the group when it is not the default, and the
/// labels of exactly the checkboxes to leave checked. Without `submit` the form is left open and
/// filled, so its frame shows the form itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetCreateStep {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub groups: Vec<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub submit: bool,
}

impl PresetCreateStep {
    fn validate(&self) -> Result<(), String> {
        text(&self.name, "preset_create name")?;
        optional_text(self.group.as_deref(), "preset_create group")?;
        self.groups
            .iter()
            .try_for_each(|label| text(label, "preset_create groups"))
    }
}

/// A secret a step types, such as a module's key. It is zeroed when dropped, never printed by
/// `Debug`, and [`Step::kept`] writes it as [`REDACTED`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(zeroize::Zeroizing<String>);

impl Secret {
    pub fn new(text: String) -> Self {
        Self(zeroize::Zeroizing::new(text))
    }

    /// The text, for the one request that stores it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for Secret {
    /// Read as any value first, so a secret of the wrong type is refused without the refusal
    /// quoting it, as serde's own type errors would.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::String(text) => Ok(Self::new(text)),
            _ => Err(D::Error::custom("a secret is text")),
        }
    }
}

/// One capability gesture: which module, what, and whether its frame waits for the jobs it starts.
///
/// On the wire the gesture is one key beside `module` and `wait`:
/// `{"module": M, "consent": "allow", "wait": false}`. No refusal echoes a value, because a secret
/// step's value must reach nothing but the one request that stores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CapabilityWire", into = "CapabilityWire")]
pub struct CapabilityStep {
    pub module: String,
    pub action: CapabilityAction,
    /// `false` captures while a job the step started is still running.
    pub wait: bool,
}

/// What a capability step does, each through the messages its control sends. Profiles and grants
/// are named by their position in the lists the section shows.
#[derive(Clone, Debug, PartialEq)]
pub enum CapabilityAction {
    /// Open the status or the settings sub-view.
    Section(CapabilitySection),
    Set {
        field: String,
        value: Value,
        profile: Option<usize>,
    },
    /// Replace a secret through its masked input.
    Secret {
        field: String,
        value: Secret,
        profile: Option<usize>,
    },
    CreateProfile {
        adapter: String,
        label: String,
    },
    RemoveProfile(usize),
    Install(String),
    Remove(String),
    Activate(bool),
    Task(String),
    /// Allow (`true`) or Don't allow on the open consent notice.
    Consent(bool),
    /// Apply the newest task result that declares an apply action.
    Apply,
    /// Cancel the module's newest live job.
    Cancel,
    Revoke(usize),
    /// Capture once every job the desktop tracks for the module has finished.
    Settle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilitySection {
    Status,
    Settings,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityWire {
    module: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    wait: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    section: Option<CapabilitySection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    set: Option<SetWire<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<SetWire<Secret>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<ProfileWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    install: Option<ResourceWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remove: Option<ResourceWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<TaskWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consent: Option<Consent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apply: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancel: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoke: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settle: Option<True>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetWire<T> {
    field: String,
    value: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<usize>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
enum ProfileWire {
    Create { adapter: String, label: String },
    Remove(usize),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceWire {
    resource: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskWire {
    task: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Consent {
    Allow,
    Deny,
}

/// The gesture keys of a capability step, of which a step takes exactly one.
const CAPABILITY_GESTURES: &str = "section, set, secret, profile, install, remove, activate, task, consent, apply, cancel, revoke or settle";

impl TryFrom<CapabilityWire> for CapabilityStep {
    type Error = String;

    fn try_from(wire: CapabilityWire) -> Result<Self, String> {
        let mut actions = Vec::new();
        if let Some(section) = wire.section {
            actions.push(CapabilityAction::Section(section));
        }
        if let Some(set) = wire.set {
            actions.push(CapabilityAction::Set {
                field: set.field,
                value: set.value,
                profile: set.profile,
            });
        }
        if let Some(secret) = wire.secret {
            actions.push(CapabilityAction::Secret {
                field: secret.field,
                value: secret.value,
                profile: secret.profile,
            });
        }
        match wire.profile {
            Some(ProfileWire::Create { adapter, label }) => {
                actions.push(CapabilityAction::CreateProfile { adapter, label });
            }
            Some(ProfileWire::Remove(index)) => {
                actions.push(CapabilityAction::RemoveProfile(index))
            }
            None => {}
        }
        if let Some(install) = wire.install {
            actions.push(CapabilityAction::Install(install.resource));
        }
        if let Some(remove) = wire.remove {
            actions.push(CapabilityAction::Remove(remove.resource));
        }
        if let Some(on) = wire.activate {
            actions.push(CapabilityAction::Activate(on));
        }
        if let Some(task) = wire.task {
            actions.push(CapabilityAction::Task(task.task));
        }
        if let Some(consent) = wire.consent {
            actions.push(CapabilityAction::Consent(matches!(consent, Consent::Allow)));
        }
        if wire.apply.is_some() {
            actions.push(CapabilityAction::Apply);
        }
        if wire.cancel.is_some() {
            actions.push(CapabilityAction::Cancel);
        }
        if let Some(index) = wire.revoke {
            actions.push(CapabilityAction::Revoke(index));
        }
        if wire.settle.is_some() {
            actions.push(CapabilityAction::Settle);
        }
        let (Some(action), 1) = (actions.pop(), actions.len() + 1) else {
            return Err(format!(
                "capability takes exactly one of {CAPABILITY_GESTURES}"
            ));
        };
        Ok(Self {
            module: wire.module,
            action,
            wait: wire.wait,
        })
    }
}

impl From<CapabilityStep> for CapabilityWire {
    fn from(step: CapabilityStep) -> Self {
        let mut wire = Self {
            module: step.module,
            wait: step.wait,
            section: None,
            set: None,
            secret: None,
            profile: None,
            install: None,
            remove: None,
            activate: None,
            task: None,
            consent: None,
            apply: None,
            cancel: None,
            revoke: None,
            settle: None,
        };
        match step.action {
            CapabilityAction::Section(section) => wire.section = Some(section),
            CapabilityAction::Set {
                field,
                value,
                profile,
            } => {
                wire.set = Some(SetWire {
                    field,
                    value,
                    profile,
                });
            }
            CapabilityAction::Secret {
                field,
                value,
                profile,
            } => {
                wire.secret = Some(SetWire {
                    field,
                    value,
                    profile,
                });
            }
            CapabilityAction::CreateProfile { adapter, label } => {
                wire.profile = Some(ProfileWire::Create { adapter, label });
            }
            CapabilityAction::RemoveProfile(index) => {
                wire.profile = Some(ProfileWire::Remove(index));
            }
            CapabilityAction::Install(resource) => wire.install = Some(ResourceWire { resource }),
            CapabilityAction::Remove(resource) => wire.remove = Some(ResourceWire { resource }),
            CapabilityAction::Activate(on) => wire.activate = Some(on),
            CapabilityAction::Task(task) => wire.task = Some(TaskWire { task }),
            CapabilityAction::Consent(allow) => {
                wire.consent = Some(if allow { Consent::Allow } else { Consent::Deny });
            }
            CapabilityAction::Apply => wire.apply = Some(True),
            CapabilityAction::Cancel => wire.cancel = Some(True),
            CapabilityAction::Revoke(index) => wire.revoke = Some(index),
            CapabilityAction::Settle => wire.settle = Some(True),
        }
        wire
    }
}

impl CapabilityStep {
    fn validate(&self) -> Result<(), String> {
        text(&self.module, "capability module")?;
        match &self.action {
            CapabilityAction::Set { field, .. } => text(field, "capability set field"),
            CapabilityAction::Secret { field, value, .. } => {
                text(field, "capability secret field")?;
                if value.expose().is_empty() {
                    return Err("capability secret needs a text value".into());
                }
                Ok(())
            }
            CapabilityAction::CreateProfile { adapter, label } => {
                text(adapter, "capability profile create adapter")?;
                text(label, "capability profile create label")
            }
            CapabilityAction::Install(resource) | CapabilityAction::Remove(resource) => {
                text(resource, "capability resource")
            }
            CapabilityAction::Task(task) => text(task, "capability task"),
            _ => Ok(()),
        }
    }
}

/// How a script names a mask, a component or a stroke.
///
/// `mask.create-<kind>` assigns the identity, so a script that creates a mask in one step has no
/// identity to write into the next one. The display name the host gave it — `Mask 1`, `Linear 1` —
/// is what a script written before the run can put there instead, resolved against the listing the
/// editor is holding when the step runs; a name that matches nothing, or more than one row, fails
/// the step rather than guessing. A position in the list is the third spelling, for the steps that
/// drive the panel as a pointer does, where the row and not its name is what the gesture means.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    untagged,
    deny_unknown_fields,
    expecting = "an identity, {\"name\": \"…\"} or a position in the list"
)]
pub enum Reference {
    /// The identity itself, written as a plain string.
    Id(String),
    /// The name the host gave it, written as `{"name": "…"}`.
    Name { name: String },
    /// A position in the list the panel shows, written as a bare integer.
    Index(usize),
}

impl Reference {
    /// A reference by the name the host gave it.
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name { name: name.into() }
    }

    /// A reference read from a request's own parameters, checked as a step's is.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let reference: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        reference.validate()?;
        Ok(reference)
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Id(id) => text(id, "an identity"),
            Self::Name { name } => text(name, "a reference's name"),
            Self::Index(_) => Ok(()),
        }
    }
}

fn optional_reference(reference: Option<&Reference>) -> Result<(), String> {
    reference.map_or(Ok(()), Reference::validate)
}

/// What one `mask` step does to the Masks panel or to the shape on the canvas.
///
/// Each verb is one of the panel's own gestures, so a script drives it through exactly the messages
/// a pointer sends and the requests that reach the owner are the panel's own. Geometry travels in
/// normalized content coordinates, which is exactly what the canvas publishes after mapping the
/// pointer through `render.transform`'s affine.
///
/// One verb per step, and one captured frame per step, so a frame is evidence of one thing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MaskStep {
    /// Open one mask, as clicking its row does.
    Select(Reference),
    /// Select one component of the open mask, which shows its handles and its number fields, or
    /// clear the selection with `null`.
    SelectComponent(Option<Reference>),
    /// Put the pointer on that component's row, or take it off the list with `null`. While a row
    /// is hovered the overlay shows that component's own contribution instead of the composed mask.
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
    /// brush on it in the chosen mode; or on one component, which appends to that brush.
    Paint(PaintStep),
    /// One change to the brush the next stroke will be drawn with.
    Brush(BrushStep),
    /// Paint one stroke into the open gesture: a press, a move per position and, unless the step
    /// says to leave it down, a release that commits it as **one** history entry.
    Stroke {
        points: Vec<[f64; 2]>,
        #[serde(default = "yes")]
        release: bool,
        /// Send one position per this many milliseconds, in real time, instead of the whole path in
        /// one update, which is what a **latency** measurement needs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
        /// Wait for each position's own drafted frame to reach the screen before sending the next
        /// one, instead of trusting `interval_ms` alone to outrun the render pipeline. A host under
        /// load can take longer than the interval to answer a position, and every later position
        /// then supersedes the one before it reaches the screen — a latency measurement that
        /// depends on at least one paired frame needs this held to true rather than assumed.
        /// Rejected without `interval_ms`, since it paces nothing on its own.
        #[serde(default, skip_serializing_if = "is_false")]
        settle_between: bool,
    },
    /// A whole shape drawn in one stroke: the press at `from`, the pointer at `to`. The pointer is
    /// still down afterwards, exactly as it is mid-drag, so the release is a step of its own.
    Sweep { from: [f64; 2], to: [f64; 2] },
    /// The pointer lifted. The shape it drew stays; committing it is a separate decision.
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Release,
    /// A press on one drawn handle, the points it is dragged through, and its release.
    Drag {
        handle: DragHandle,
        points: Vec<[f64; 2]>,
    },
    /// Commit the open gesture: one history entry.
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Apply,
    /// Discard the open gesture.
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Cancel,
    /// One component row's own list edit, on the row it names rather than on whichever component
    /// happens to be selected.
    Row(MaskRow),
    /// Enter or leave the host's own canvas pick for the selected component's kind, which is what
    /// the panel's Pick button does.
    #[serde(deserialize_with = "only_true", serialize_with = "write_true")]
    Pick,
}

impl MaskStep {
    /// A stroke through `points`, sent in one update and released unless `release` is false.
    pub fn stroke(points: impl Into<Vec<[f64; 2]>>, release: bool) -> Self {
        Self::Stroke {
            points: points.into(),
            release,
            interval_ms: None,
            settle_between: false,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Select(reference) | Self::EditShape(reference) => reference.validate(),
            Self::SelectComponent(reference) | Self::Hover(reference) => {
                optional_reference(reference.as_ref())
            }
            Self::Mode(word) => text(word, "mask mode"),
            Self::New(kind) | Self::Add(kind) => text(kind, "a mask component kind"),
            Self::Paint(PaintStep::Component(reference)) => reference.validate(),
            Self::Paint(_) => Ok(()),
            Self::Brush(brush) => brush.validate(),
            Self::Stroke {
                points,
                interval_ms,
                settle_between,
                ..
            } => {
                if points.is_empty() {
                    return Err("mask stroke needs at least one point".into());
                }
                points
                    .iter()
                    .try_for_each(|point| content_point(*point, "mask stroke point"))?;
                positive(*interval_ms, "mask stroke interval_ms")?;
                if *settle_between && interval_ms.is_none() {
                    return Err("mask stroke settle_between needs interval_ms".into());
                }
                Ok(())
            }
            Self::Sweep { from, to } => {
                content_point(*from, "mask sweep from")?;
                content_point(*to, "mask sweep to")
            }
            Self::Drag { points, .. } => {
                if points.is_empty() {
                    return Err("mask drag needs at least one point".into());
                }
                points
                    .iter()
                    .try_for_each(|point| content_point(*point, "mask drag point"))
            }
            Self::Row(row) => row.validate(),
            Self::Release | Self::Apply | Self::Cancel | Self::Pick => Ok(()),
        }
    }
}

/// One drawn handle of a gesture's figure, by the name the design draws it under. The create
/// gesture's own grab is not among them: it is not drawn, and a sweep is how a script uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DragHandle {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "middle")]
    Middle,
    #[serde(rename = "end")]
    End,
    #[serde(rename = "centre")]
    Centre,
    #[serde(rename = "radius+x")]
    RadiusPlusX,
    #[serde(rename = "radius-x")]
    RadiusMinusX,
    #[serde(rename = "radius+y")]
    RadiusPlusY,
    #[serde(rename = "radius-y")]
    RadiusMinusY,
    #[serde(rename = "rotation")]
    Rotation,
    #[serde(rename = "feather")]
    Feather,
}

/// What the next stroke will land on, named before the gesture rather than guessed from where the
/// pointer went down: `"new-mask"`, `"new-brush"` or `{"component": REF}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaintStep {
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
///
/// There is no density: it would make coverage depend on stamp spacing and therefore on
/// resolution. Flow is the per-stroke amount, and a script naming a density is refused as an
/// unknown field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feather: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erase: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erase_held: Option<bool>,
    /// Hold the next stroke to the colour under the brush where it begins. The script sets the flag
    /// and never a colour: the host reads the pixel the masked operation receives at the stroke's
    /// first position, exactly as it does for a pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_to_colour: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour_refine: Option<f64>,
    /// `[FIELD, STEPS]`: move one declared field by that many of its own declared steps, which is
    /// what a bracket key does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nudge: Option<(String, f64)>,
}

impl BrushStep {
    fn validate(&self) -> Result<(), String> {
        if *self == Self::default() {
            return Err(
                "mask brush takes size, feather, flow, erase, erase_held, limit_to_colour, \
                 colour_refine or nudge: [FIELD, STEPS]"
                    .into(),
            );
        }
        if let Some((field, _)) = &self.nudge {
            text(field, "mask brush nudge field")?;
        }
        Ok(())
    }
}

/// One component row's control, by the row it belongs to and the one value it changes:
/// `{"component": REF, "mode": "subtract"}`, or `"invert": true`, `"index": N`, `"delete": true` or
/// `"delete_stroke": REF`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RowWire", into = "RowWire")]
pub struct MaskRow {
    pub component: Reference,
    pub edit: RowStep,
}

/// What one component row's control does, before its row is resolved to an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowStep {
    Mode(String),
    Invert(bool),
    Move(usize),
    Delete,
    /// Remove one of this component's strokes, named the same three ways every other object in a
    /// script is: its content address, the label the row shows — `Stroke 1` — or its position in
    /// the list.
    DeleteStroke(Reference),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowWire {
    component: Reference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invert: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delete: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delete_stroke: Option<Reference>,
}

impl TryFrom<RowWire> for MaskRow {
    type Error = String;

    fn try_from(wire: RowWire) -> Result<Self, String> {
        let edits = [
            wire.mode.map(RowStep::Mode),
            wire.invert.map(RowStep::Invert),
            wire.index.map(RowStep::Move),
            wire.delete.map(|True| RowStep::Delete),
            wire.delete_stroke.map(RowStep::DeleteStroke),
        ];
        let mut edits = edits.into_iter().flatten();
        match (edits.next(), edits.next()) {
            (Some(edit), None) => Ok(Self {
                component: wire.component,
                edit,
            }),
            _ => Err(
                "mask row takes a component and one of mode, invert, index, delete or delete_stroke"
                    .into(),
            ),
        }
    }
}

impl From<MaskRow> for RowWire {
    fn from(row: MaskRow) -> Self {
        let mut wire = Self {
            component: row.component,
            mode: None,
            invert: None,
            index: None,
            delete: None,
            delete_stroke: None,
        };
        match row.edit {
            RowStep::Mode(mode) => wire.mode = Some(mode),
            RowStep::Invert(invert) => wire.invert = Some(invert),
            RowStep::Move(index) => wire.index = Some(index),
            RowStep::Delete => wire.delete = Some(True),
            RowStep::DeleteStroke(stroke) => wire.delete_stroke = Some(stroke),
        }
        wire
    }
}

impl MaskRow {
    fn validate(&self) -> Result<(), String> {
        self.component.validate()?;
        match &self.edit {
            RowStep::Mode(mode) => text(mode, "mask row mode"),
            RowStep::DeleteStroke(stroke) => stroke.validate(),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
