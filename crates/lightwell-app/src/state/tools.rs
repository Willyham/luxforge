//! The tools panel model: the descriptor-to-control mapping and one section per registered module.
//! A section keeps an input digest and a version, so a field change in one module re-derives that
//! module alone and leaves every other section untouched.
use crate::{
    app::{
        fields::{
            action_params, channel_text, field_id, labelled, number_text, parse_field,
            undeclared_label, unsupported_label,
        },
        message::{MenuTarget, PaletteAction},
    },
    crop_draft::{AspectPreset, CropDraft},
    state::Inputs,
};
use lightwell_core::{
    ActionDescriptor, CanvasInteraction, Control, CropPayload, EffectStage, Layer,
    ModuleDescriptor, ORIENTATION_EFFECT, Orientation, ParameterDescriptor, ParameterKind,
    ResetAction,
};
use serde_json::{Map, Value};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

/// What the panel says about discovery before any section exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ToolsStatus {
    #[default]
    Loading,
    Empty,
    Ready,
}

impl ToolsStatus {
    /// The line shown in place of the sections.
    pub(crate) fn message(self) -> Option<&'static str> {
        match self {
            Self::Loading => Some("Loading tool modules…"),
            Self::Empty => Some("No tool modules are available"),
            Self::Ready => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ToolsModel {
    pub(crate) sections: Vec<SectionModel>,
    /// Proof and diagnostic modules, listed only when the desktop was started with `--developer`.
    pub(crate) developer: Vec<SectionModel>,
    pub(crate) status: ToolsStatus,
    /// The inline menu open on one generated control or the crop draft's Apply, if any.
    pub(crate) menu: Option<MenuTarget>,
}

/// A declared action with fixed parameters, as a header or group reset button raises it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResetRef {
    pub(crate) action: String,
    pub(crate) preset: Map<String, Value>,
}

impl ResetRef {
    fn of(reset: Option<&ResetAction>) -> Option<Self> {
        reset.map(|reset| Self {
            action: reset.action.clone(),
            preset: reset.preset.clone(),
        })
    }
}

/// One registered module's section. `version` increases only when the section's own inputs change.
#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SectionModel {
    pub(crate) module_id: String,
    pub(crate) title: String,
    pub(crate) hint: Option<String>,
    pub(crate) expanded: bool,
    /// A non-neutral layer of this module is in the current recipe.
    pub(crate) active: bool,
    pub(crate) unavailable: Option<String>,
    pub(crate) reset: Option<ResetRef>,
    pub(crate) controls: Vec<ControlModel>,
    pub(crate) version: u64,
    pub(crate) enabled: bool,
    /// Why editing is disabled, in the words the status bar would use.
    pub(crate) disabled_reason: Option<String>,
    /// The inputs this section was derived from.
    digest: u64,
}

/// What a value control shows while it is being typed: the text as typed, not the formatted value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ValueEdit {
    #[default]
    None,
    Typing(String),
}

impl ValueEdit {
    /// The text a field shows: what is being typed, else the formatted value.
    pub(crate) fn text<'a>(&'a self, display: &'a str) -> &'a str {
        match self {
            Self::Typing(text) => text,
            Self::None => display,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) unit: Option<String>,
    pub(crate) min: f64,
    pub(crate) max: f64,
    /// Where a bipolar fill starts.
    pub(crate) zero: f64,
    pub(crate) value: f64,
    /// The formatted value, or the text as typed when it cannot be read.
    pub(crate) display: String,
    pub(crate) edit: ValueEdit,
    pub(crate) dragging: bool,
    pub(crate) integer: bool,
    /// The declared range, when the text does not satisfy it.
    pub(crate) invalid: Option<String>,
    /// The parameter's declared default, already formatted: what a reset sets the field to.
    pub(crate) default: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnumControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) options: Vec<String>,
    pub(crate) selected: Option<usize>,
    /// A short option list is a segmented control rather than a menu.
    pub(crate) segmented: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColorControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) ids: [String; 3],
    pub(crate) label: String,
    pub(crate) channels: [String; 3],
    /// The whole field's text, so one channel edit keeps the other two as typed.
    pub(crate) text: String,
    pub(crate) invalid: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupControl {
    pub(crate) label: String,
    pub(crate) reset: Option<ResetRef>,
    /// The group's position inside its module's controls, so a reset names it without a search.
    pub(crate) path: Vec<usize>,
    pub(crate) controls: Vec<ControlModel>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionControl {
    pub(crate) action: String,
    pub(crate) label: String,
    pub(crate) preset: Map<String, Value>,
    pub(crate) runnable: bool,
    /// Why the action cannot run, when it cannot.
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlModel {
    Slider(SliderControl),
    Enum(EnumControl),
    Color(ColorControl),
    Group(GroupControl),
    Action(ActionControl),
    /// A control this build cannot draw keeps its name on screen rather than disappearing.
    Unsupported(String),
    /// The host's crop-frame editor, at the top of the declaring module's section.
    CropFrame(Box<CropSectionModel>),
}

/// One generated ratio preset button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetChip {
    pub(crate) index: usize,
    pub(crate) label: String,
    pub(crate) chosen: bool,
}

/// The crop draft's own controls, rendered by the host for a declared crop-frame interaction.
#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CropSectionModel {
    pub(crate) title: String,
    /// A draft is open.
    pub(crate) drafting: bool,
    /// The truncated preview that opens a draft is in flight.
    pub(crate) pending: bool,
    pub(crate) conflicted: bool,
    /// A historical preview is shown, so the draft is paused rather than discarded.
    pub(crate) paused: bool,
    pub(crate) presets: Vec<PresetChip>,
    pub(crate) custom: (String, String),
    pub(crate) custom_ids: (String, String),
    pub(crate) lock_label: String,
    pub(crate) can_swap: bool,
    pub(crate) angle: String,
    pub(crate) angle_id: String,
    pub(crate) guide: bool,
    /// How far one nudge button moves the angle, in degrees.
    pub(crate) nudge: f64,
    /// The draft's own numbers, so what is on screen is observable without a debugger.
    pub(crate) readout: Vec<String>,
    pub(crate) can_start: bool,
    pub(crate) can_apply: bool,
    pub(crate) can_reapply: bool,
    pub(crate) enabled: bool,
}

impl ToolsModel {
    /// Recompute every section whose inputs changed and leave the rest exactly as they were.
    pub(crate) fn refresh(&mut self, inputs: &Inputs<'_>) {
        self.menu = inputs.menu.cloned();
        self.status = match (inputs.modules.is_empty(), inputs.modules_ready) {
            (true, false) => ToolsStatus::Loading,
            (true, true) => ToolsStatus::Empty,
            (false, _) => ToolsStatus::Ready,
        };
        let mut sections = Vec::new();
        let mut developer = Vec::new();
        for module in inputs.modules {
            if module.id == "lightwell.raw"
                && !inputs.state.is_some_and(|state| {
                    matches!(state.asset.source, lightwell_core::SourceKind::Raw { .. })
                })
            {
                continue;
            }
            if module.developer && !inputs.developer {
                continue;
            }
            let target = if module.developer {
                &mut developer
            } else {
                &mut sections
            };
            let previous = self
                .sections
                .iter()
                .chain(self.developer.iter())
                .find(|section| section.module_id == module.id);
            target.push(section(module, inputs, previous));
        }
        self.sections = sections;
        self.developer = developer;
    }

    /// Every section in registry order, developer sections last.
    pub(crate) fn all(&self) -> impl Iterator<Item = &SectionModel> {
        self.sections.iter().chain(self.developer.iter())
    }
}

/// One module's section, re-derived only when its own inputs changed.
fn section(
    module: &ModuleDescriptor,
    inputs: &Inputs<'_>,
    previous: Option<&SectionModel>,
) -> SectionModel {
    let expanded = expanded(module, inputs);
    let unavailable = match &module.availability {
        lightwell_core::Availability::Available => None,
        lightwell_core::Availability::Unavailable { reason } => Some(reason.clone()),
    };
    let disabled_reason = disabled_reason(unavailable.as_deref(), inputs);
    let enabled = disabled_reason.is_none();
    let active = active(module, inputs);
    let digest = digest(module, inputs, expanded, enabled, active);
    if let Some(previous) = previous
        && previous.digest == digest
    {
        return previous.clone();
    }
    let mut controls = Vec::new();
    // A declared crop frame is a host interaction, not a control: the host renders its draft panel
    // here and the module's own controls, Reset crop included, still come below.
    if let Some(frame) = crop_frame(inputs.modules).filter(|frame| frame.module.id == module.id) {
        controls.push(ControlModel::CropFrame(Box::new(crop_section(
            &frame, inputs, enabled,
        ))));
    }
    for (index, control) in module.controls.iter().enumerate() {
        controls.push(control_model(module, control, inputs, enabled, &[index]));
    }
    SectionModel {
        module_id: module.id.clone(),
        title: module.title.clone(),
        hint: module.hint.clone(),
        expanded,
        active,
        unavailable,
        // A reset is a mutation, so a section that cannot edit (a historical preview, a request in
        // flight, a missing provider) offers none at all rather than a dimmed one.
        reset: enabled
            .then(|| ResetRef::of(module.reset.as_ref()))
            .flatten(),
        controls,
        version: previous.map(|previous| previous.version + 1).unwrap_or(1),
        enabled,
        disabled_reason,
        digest,
    }
}

/// Sections start expanded except developer ones, and the section whose canvas mode is drafting is
/// held open until the draft ends.
fn expanded(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> bool {
    if inputs.draft.is_some() && owns_mode(module, inputs) {
        return true;
    }
    inputs
        .expanded
        .get(&module.id)
        .copied()
        .unwrap_or(!module.developer)
}

/// This module owns the active canvas mode.
fn owns_mode(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> bool {
    module.canvas.is_some() && inputs.session.workspace.mode == module.id
}

/// Why editing this module is disabled, in the words the status bar would use.
fn disabled_reason(unavailable: Option<&str>, inputs: &Inputs<'_>) -> Option<String> {
    if let Some(reason) = unavailable {
        return Some(reason.to_owned());
    }
    if inputs.state.is_none() {
        return Some("No photograph is open".into());
    }
    if !inputs.session.preview.can_edit() {
        return Some("Return to current to edit".into());
    }
    inputs
        .busy
        .then(|| "Waiting for the last request".to_owned())
}

/// The current recipe holds a layer of one of this module's effects, and that layer does something.
fn active(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> bool {
    let Some(state) = inputs.state else {
        return false;
    };
    state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .any(|layer| {
            module
                .effects
                .iter()
                .any(|effect| effect.id == layer.effect_id)
                && !neutral(module, layer)
        })
}

/// A neutral layer is stored but changes nothing, so it is not an edit. Two stored payloads have a
/// neutral form: the orientation layer's identity, which is what four quarter turns leave behind,
/// and a crop-frame module's whole image. A payload with no neutral form is always an edit.
fn neutral(module: &ModuleDescriptor, layer: &Layer) -> bool {
    if layer.effect_id == ORIENTATION_EFFECT {
        return serde_json::from_value::<Orientation>(layer.payload.clone())
            .map(|orientation| orientation == Orientation::NEUTRAL)
            .unwrap_or(false);
    }
    if !matches!(module.canvas, Some(CanvasInteraction::CropFrame { .. })) {
        return false;
    }
    serde_json::from_value::<CropPayload>(layer.payload.clone())
        .map(|payload| payload == CropPayload::NEUTRAL)
        .unwrap_or(false)
}

/// Everything this section is derived from, so an unrelated change leaves its version alone.
fn digest(
    module: &ModuleDescriptor,
    inputs: &Inputs<'_>,
    expanded: bool,
    enabled: bool,
    active: bool,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    module.id.hash(&mut hasher);
    format!("{:?}", module.availability).hash(&mut hasher);
    (expanded, enabled, active, inputs.developer).hash(&mut hasher);
    for action in &module.actions {
        action.id.hash(&mut hasher);
        for parameter in &action.parameters {
            inputs
                .fields
                .get(&action.id, &parameter.name)
                .hash(&mut hasher);
        }
        // Which of this module's fields is being typed or dragged changes only this section.
        for target in [inputs.editing, inputs.dragging] {
            target
                .filter(|(declared, _)| *declared == action.id)
                .map(|(declared, parameter)| (declared.as_str(), parameter.as_str()))
                .hash(&mut hasher);
        }
        // A slider gesture belongs to the module whose action it drafts, so its conflict state
        // reaches that section alone.
        inputs
            .slider_draft
            .filter(|draft| draft.action == action.id)
            .map(|draft| (draft.parameter.as_str(), draft.conflicted))
            .hash(&mut hasher);
    }
    if let Some(state) = inputs.state {
        for layer in &state.current_entry.snapshot.recipe.layers {
            if module
                .effects
                .iter()
                .any(|effect| effect.id == layer.effect_id)
            {
                layer.id.as_str().hash(&mut hasher);
                layer.payload.to_string().hash(&mut hasher);
            }
        }
    }
    if owns_mode(module, inputs)
        || matches!(module.canvas, Some(CanvasInteraction::CropFrame { .. }))
    {
        draft_digest(inputs).hash(&mut hasher);
    }
    hasher.finish()
}

/// Everything the crop section shows, as one string. The draft is transient state, so a section
/// that owns the canvas mode follows it.
fn draft_digest(inputs: &Inputs<'_>) -> String {
    match inputs.draft {
        Some(draft) => format!(
            "{}|{}|{}|{}|{}|{}|{}",
            draft.summary(),
            draft.preset,
            inputs.crop_angle,
            inputs.crop_custom.0,
            inputs.crop_custom.1,
            inputs.crop_guide,
            inputs.session.preview.can_edit(),
        ),
        None => format!("none|{}", inputs.draft_pending),
    }
}

/// One declared control as the panel models it.
fn control_model(
    module: &ModuleDescriptor,
    control: &Control,
    inputs: &Inputs<'_>,
    enabled: bool,
    path: &[usize],
) -> ControlModel {
    match classify(control) {
        Rendered::Group {
            label,
            controls,
            reset,
        } => ControlModel::Group(GroupControl {
            label: label.to_owned(),
            reset: ResetRef::of(reset),
            path: path.to_vec(),
            controls: controls
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let mut child_path = path.to_vec();
                    child_path.push(index);
                    control_model(module, child, inputs, enabled, &child_path)
                })
                .collect(),
        }),
        Rendered::Number {
            action,
            parameter,
            label,
        }
        | Rendered::Color {
            action,
            parameter,
            label,
        } => value_model(module, inputs, action, parameter, label),
        Rendered::Action {
            action,
            label,
            preset,
        } => {
            let declared = declared_action(inputs.modules, action);
            let params = declared.map(|declared| action_params(declared, preset, inputs.fields));
            ControlModel::Action(ActionControl {
                action: action.to_owned(),
                label: label.to_owned(),
                preset: preset.clone(),
                runnable: enabled && matches!(params, Some(Ok(_))),
                reason: match params {
                    Some(Err(message)) => Some(message),
                    Some(Ok(_)) => None,
                    None => Some(format!("No module declares the action {action}")),
                },
            })
        }
        Rendered::Unsupported(kind) => ControlModel::Unsupported(unsupported_label(&kind)),
    }
}

/// A value control is modelled by the kind its parameter declares, so a descriptor that grows a
/// kind this build cannot draw is named rather than dropped.
fn value_model(
    module: &ModuleDescriptor,
    inputs: &Inputs<'_>,
    action: &str,
    parameter: &str,
    label: &str,
) -> ControlModel {
    let Some(declared) = declared_parameter(module, action, parameter) else {
        return ControlModel::Unsupported(undeclared_label(action, parameter));
    };
    let text = inputs.fields.get(action, parameter).unwrap_or_default();
    let invalid = parse_field(declared, text).err();
    let typing = inputs
        .editing
        .is_some_and(|(a, p)| a == action && p == parameter);
    match &declared.kind {
        ParameterKind::Integer { min, max } => ControlModel::Slider(slider(
            action,
            parameter,
            label,
            declared,
            text,
            invalid,
            typing,
            inputs,
            *min as f64,
            *max as f64,
            true,
        )),
        ParameterKind::Number { min, max } => ControlModel::Slider(slider(
            action, parameter, label, declared, text, invalid, typing, inputs, *min, *max, false,
        )),
        ParameterKind::Color => ControlModel::Color(ColorControl {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            ids: [0, 1, 2].map(|index| {
                field_id(action, parameter, Some(crate::app::fields::CHANNELS[index]))
            }),
            label: labelled(label, declared),
            channels: [0, 1, 2].map(|index| channel_text(text, index).to_owned()),
            text: text.to_owned(),
            invalid,
        }),
        ParameterKind::Enum { options } => ControlModel::Enum(EnumControl {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            id: field_id(action, parameter, None),
            label: labelled(label, declared),
            options: options.clone(),
            selected: options.iter().position(|option| option == text.trim()),
            segmented: options.len() <= 4,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn slider(
    action: &str,
    parameter: &str,
    label: &str,
    declared: &ParameterDescriptor,
    text: &str,
    invalid: Option<String>,
    typing: bool,
    inputs: &Inputs<'_>,
    min: f64,
    max: f64,
    integer: bool,
) -> SliderControl {
    let value = parse_field(declared, text)
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(min);
    SliderControl {
        action: action.to_owned(),
        parameter: parameter.to_owned(),
        id: field_id(action, parameter, None),
        label: labelled(label, declared),
        unit: declared.unit.clone(),
        min,
        max,
        zero: 0.0_f64.clamp(min, max),
        value,
        display: if invalid.is_some() {
            text.to_owned()
        } else {
            number_text(value)
        },
        edit: if typing {
            ValueEdit::Typing(text.to_owned())
        } else {
            ValueEdit::None
        },
        dragging: inputs
            .dragging
            .is_some_and(|(a, p)| a == action && p == parameter),
        integer,
        invalid,
        default: crate::app::fields::seed_text(declared),
    }
}

/// The crop draft's own panel, generated from the declared crop-frame interaction.
fn crop_section(frame: &CropFrame<'_>, inputs: &Inputs<'_>, enabled: bool) -> CropSectionModel {
    let presets = frame.presets();
    let base = CropSectionModel {
        title: frame.title.to_owned(),
        pending: inputs.draft_pending,
        paused: !inputs.session.preview.can_edit(),
        custom: (
            inputs.crop_custom.0.to_owned(),
            inputs.crop_custom.1.to_owned(),
        ),
        custom_ids: (
            field_id(frame.fit_action, "custom-width", None),
            field_id(frame.fit_action, "custom-height", None),
        ),
        angle: inputs.crop_angle.to_owned(),
        angle_id: field_id(frame.action, frame.angle, None),
        guide: inputs.crop_guide,
        nudge: crate::app::crop::ANGLE_STEP,
        enabled,
        ..CropSectionModel::default()
    };
    let Some(draft) = inputs.draft else {
        return CropSectionModel {
            can_start: enabled && !inputs.draft_pending,
            lock_label: "Lock ratio".into(),
            ..base
        };
    };
    CropSectionModel {
        drafting: true,
        conflicted: draft.conflicted,
        presets: presets
            .iter()
            .enumerate()
            .map(|(index, preset)| PresetChip {
                index,
                label: preset.label(),
                chosen: draft.preset == preset.option,
            })
            .collect(),
        lock_label: match draft.aspect.ratio() {
            Some(_) => "Unlock ratio".into(),
            None => "Lock ratio".into(),
        },
        can_swap: enabled && draft.aspect.ratio().is_some(),
        can_apply: enabled && !draft.conflicted,
        can_reapply: !inputs.busy,
        readout: readout(draft),
        ..base
    }
}

/// The draft's own numbers, in the order the panel prints them.
fn readout(draft: &CropDraft) -> Vec<String> {
    let payload = draft.payload();
    let output = match draft.output() {
        Ok(rect) => format!(
            "{} × {} px at ({}, {})",
            rect.width, rect.height, rect.x, rect.y
        ),
        Err(error) => error.detail.clone(),
    };
    vec![format!(
        "Input stage {} × {} · box {:.0} × {:.0}\nRect {:.0}, {:.0}, {:.0} × {:.0} box px\nOutput {output}\nPayload angle {} x {:.6} y {:.6} w {:.6} h {:.6}",
        draft.stage.width,
        draft.stage.height,
        draft.stage.bounding_box().0,
        draft.stage.bounding_box().1,
        draft.rect.x,
        draft.rect.y,
        draft.rect.width,
        draft.rect.height,
        number_text(payload.angle),
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    )]
}

// ---- descriptor mapping ------------------------------------------------------------------------

/// What the desktop makes of one declared control. A kind this build cannot draw keeps its name on
/// screen rather than disappearing from the panel.
pub(crate) enum Rendered<'a> {
    Group {
        label: &'a str,
        controls: &'a [Control],
        reset: Option<&'a ResetAction>,
    },
    Number {
        action: &'a str,
        parameter: &'a str,
        label: &'a str,
    },
    Color {
        action: &'a str,
        parameter: &'a str,
        label: &'a str,
    },
    Action {
        action: &'a str,
        label: &'a str,
        preset: &'a Map<String, Value>,
    },
    Unsupported(String),
}

pub(crate) fn classify(control: &Control) -> Rendered<'_> {
    match control {
        Control::Group {
            label,
            controls,
            reset,
        } => Rendered::Group {
            label,
            controls,
            reset: reset.as_ref(),
        },
        Control::Number {
            action,
            parameter,
            label,
        } => Rendered::Number {
            action,
            parameter,
            label,
        },
        Control::Color {
            action,
            parameter,
            label,
        } => Rendered::Color {
            action,
            parameter,
            label,
        },
        Control::Action {
            action,
            label,
            preset,
        } => Rendered::Action {
            action,
            label,
            preset,
        },
        // A kind added to the descriptor later is reported, never dropped.
        #[allow(unreachable_patterns)]
        other => Rendered::Unsupported(control_kind(other)),
    }
}

/// The descriptor's own kind tag, so an unrenderable control can still be named.
pub(crate) fn control_kind(control: &Control) -> String {
    serde_json::to_value(control)
        .ok()
        .and_then(|value| value.get("kind").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

pub(crate) fn declared_action<'a>(
    modules: &'a [ModuleDescriptor],
    action: &str,
) -> Option<&'a ActionDescriptor> {
    modules.iter().find_map(|module| module.action(action))
}

pub(crate) fn declared_parameter<'a>(
    module: &'a ModuleDescriptor,
    action: &str,
    parameter: &str,
) -> Option<&'a ParameterDescriptor> {
    module.action(action)?.parameter(parameter)
}

/// This action merges the fields it is sent into the state it already holds, so each of its
/// generated controls submits its own parameter alone and its gesture is a draft.
pub(crate) fn is_patch(modules: &[ModuleDescriptor], action: &str) -> bool {
    declared_action(modules, action).is_some_and(|declared| declared.patch)
}

/// The label a generated control carries for one field, as the panel and the status line name it.
pub(crate) fn control_label(
    modules: &[ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<String> {
    modules
        .iter()
        .find_map(|module| labelled_control(&module.controls, action, parameter))
        .map(str::to_owned)
}

fn labelled_control<'a>(controls: &'a [Control], action: &str, parameter: &str) -> Option<&'a str> {
    controls.iter().find_map(|control| match classify(control) {
        Rendered::Group { controls, .. } => labelled_control(controls, action, parameter),
        Rendered::Number {
            action: declared,
            parameter: named,
            label,
        }
        | Rendered::Color {
            action: declared,
            parameter: named,
            label,
        } if declared == action && named == parameter => Some(label),
        _ => None,
    })
}

/// The module that declares this id, when it is registered.
pub(crate) fn module_of<'a>(
    modules: &'a [ModuleDescriptor],
    id: &str,
) -> Option<&'a ModuleDescriptor> {
    modules.iter().find(|module| module.id == id)
}

/// The first available module that declares a canvas pick: its action and coordinate parameters.
/// A crop frame is a different adapter and is ignored here rather than treated as a pick, and so is
/// a sample-apply pick, whose answer comes from a module query rather than from the coordinates
/// alone. Both still appear in the mode strip, which is derived from the declaration itself.
pub(crate) fn point_pick(modules: &[ModuleDescriptor]) -> Option<(&str, &str, &str)> {
    modules.iter().find_map(|module| match &module.canvas {
        Some(CanvasInteraction::PointPick { action, x, y, .. }) if module.is_available() => {
            Some((action.as_str(), x.as_str(), y.as_str()))
        }
        Some(CanvasInteraction::PointPick { .. })
        | Some(CanvasInteraction::SampleApply { .. })
        | Some(CanvasInteraction::CropFrame { .. })
        | None => None,
    })
}

/// Resolve a point interaction from the selected canvas tool. The pointer retains its existing
/// first-pick behavior for the developer pixel proof; selecting RAW targets its sensor picker.
pub(crate) fn point_pick_for_mode<'a>(
    modules: &'a [ModuleDescriptor],
    mode: &str,
) -> Option<(&'a str, &'a str, &'a str)> {
    if mode == lightwell_core::POINTER_MODE {
        return point_pick(modules);
    }
    modules
        .iter()
        .find(|module| module.id == mode)
        .and_then(|module| match &module.canvas {
            Some(CanvasInteraction::PointPick { action, x, y, .. }) if module.is_available() => {
                Some((action.as_str(), x.as_str(), y.as_str()))
            }
            _ => None,
        })
}

/// One declared crop-frame interaction: the action Apply calls, the parameter names it fills, and
/// the fit action whose `aspect` enum generates the ratio presets. The desktop reads every name from
/// here, so it knows no tool by name.
pub(crate) struct CropFrame<'a> {
    pub(crate) module: &'a ModuleDescriptor,
    pub(crate) action: &'a str,
    pub(crate) angle: &'a str,
    x: &'a str,
    y: &'a str,
    width: &'a str,
    height: &'a str,
    pub(crate) fit_action: &'a str,
    aspect: &'a str,
    pub(crate) title: &'a str,
}

impl CropFrame<'_> {
    /// The durable effect identity of the crop layer: the module's geometry effect.
    pub(crate) fn effect(&self) -> Option<&str> {
        self.module
            .effects
            .iter()
            .find(|effect| effect.stage == EffectStage::Geometry)
            .map(|effect| effect.id.as_str())
    }

    /// The ratio presets, generated from the fit action's declared `aspect` options.
    pub(crate) fn presets(&self) -> Vec<AspectPreset> {
        match self
            .module
            .action(self.fit_action)
            .and_then(|action| action.parameter(self.aspect))
            .map(|parameter| &parameter.kind)
        {
            Some(ParameterKind::Enum { options }) => crate::crop_draft::aspect_presets(options),
            _ => Vec::new(),
        }
    }

    /// The payload as request fields under the declared parameter names.
    pub(crate) fn params(&self, payload: &CropPayload) -> Map<String, Value> {
        [
            (self.angle, payload.angle),
            (self.x, payload.x),
            (self.y, payload.y),
            (self.width, payload.width),
            (self.height, payload.height),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), Value::from(value)))
        .collect()
    }
}

/// The first available module that declares a crop frame.
pub(crate) fn crop_frame(modules: &[ModuleDescriptor]) -> Option<CropFrame<'_>> {
    modules.iter().find_map(|module| match &module.canvas {
        Some(CanvasInteraction::CropFrame {
            action,
            angle,
            x,
            y,
            width,
            height,
            fit_action,
            aspect,
            title,
            ..
        }) if module.is_available() => Some(CropFrame {
            module,
            action,
            angle,
            x,
            y,
            width,
            height,
            fit_action,
            aspect,
            title,
        }),
        _ => None,
    })
}

/// Every module-declared palette entry the registry offers: each generated control action
/// ("`<module title> · <control label>`"), each module's own reset and each available canvas mode
/// ("`Mode · <title>`"). Unfiltered and in registry order; `state::palette` combines these with the
/// host commands and applies the query once, over the whole list. A developer module (the pixel
/// proof) is listed only when the run asked for it, exactly as its section is.
pub(crate) fn palette_entries(
    modules: &[ModuleDescriptor],
    developer: bool,
) -> Vec<(String, String, PaletteAction)> {
    let mut entries = Vec::new();
    for module in modules
        .iter()
        .filter(|module| module.is_available())
        .filter(|module| !module.developer || developer)
    {
        collect_actions(module, &module.controls, &mut entries);
        if let Some(reset) = &module.reset {
            entries.push((
                format!("{} · Reset", module.title),
                format!("edit.{}", reset.action),
                PaletteAction::Run {
                    action: reset.action.clone(),
                    preset: reset.preset.clone(),
                },
            ));
        }
        if let Some(canvas) = &module.canvas {
            entries.push((
                format!("Mode · {}", canvas.title()),
                "workspace.set".to_owned(),
                PaletteAction::Mode(module.id.clone()),
            ));
        }
    }
    entries
}

fn collect_actions(
    module: &ModuleDescriptor,
    controls: &[Control],
    entries: &mut Vec<(String, String, PaletteAction)>,
) {
    for control in controls {
        match classify(control) {
            Rendered::Group { controls, .. } => collect_actions(module, controls, entries),
            Rendered::Action {
                action,
                label,
                preset,
            } => entries.push((
                format!("{} · {label}", module.title),
                format!("edit.{action}"),
                PaletteAction::Run {
                    action: action.to_owned(),
                    preset: preset.clone(),
                },
            )),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{CROP_ASPECTS, CROP_EFFECT, crop_descriptor};
    use lightwell_core::Availability;
    use serde_json::json;

    #[test]
    fn selected_raw_canvas_mode_routes_to_its_neutral_picker() {
        let modules: Vec<_> = lightwell_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect();
        assert_eq!(
            point_pick_for_mode(&modules, "lightwell.raw"),
            Some(("pick-raw-neutral", "x", "y"))
        );
        assert_eq!(
            point_pick_for_mode(&modules, "lightwell.pixel"),
            Some(("set-pixel", "x", "y"))
        );
        assert!(point_pick_for_mode(&modules, "lightwell.crop").is_none());
    }

    #[test]
    fn the_crop_frame_and_its_presets_come_from_the_declared_descriptor() {
        let modules = vec![crop_descriptor()];
        let frame = crop_frame(&modules).expect("a declared crop frame");
        assert_eq!(frame.action, "crop");
        assert_eq!(frame.fit_action, "crop-fit");
        assert_eq!(frame.effect(), Some(CROP_EFFECT));
        let presets = frame.presets();
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.option.as_str())
                .collect::<Vec<_>>(),
            CROP_ASPECTS.to_vec(),
            "the ratio list is generated, not hard-coded"
        );
        // Apply fills exactly the names the canvas declares, with the payload's own numbers.
        let params = frame.params(&CropPayload {
            angle: -3.5,
            x: 0.25,
            y: 0.125,
            width: 0.5,
            height: 0.25,
        });
        assert_eq!(params["angle"], json!(-3.5));
        assert_eq!(params["x"], json!(0.25));
        assert_eq!(params["height"], json!(0.25));
        assert_eq!(params.len(), 5);
        // A crop frame is not a point pick, and an unavailable module declares no frame.
        assert!(point_pick(&modules).is_none());
        let unavailable = vec![ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "test".into(),
            },
            ..crop_descriptor()
        }];
        assert!(crop_frame(&unavailable).is_none());
    }
}
