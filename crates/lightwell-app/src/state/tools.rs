//! The tools panel model: the descriptor-to-control mapping and one section per registered module.
//! A section keeps an input digest and a version, so a field change in one module re-derives that
//! module alone and leaves every other section untouched.
use crate::{
    app::{
        fields::{
            action_params, channel_text, decimals_for, field_id, format_number, labelled,
            parse_field, undeclared_label, unsupported_label,
        },
        message::{MenuTarget, PaletteAction},
    },
    crop_draft::{AspectPreset, CropDraft},
    state::Inputs,
};
use lightwell_core::{
    ActionDescriptor, ActionStyle, AssetId, CanvasInteraction, ChoiceStyle, ColorStyle, Control,
    CropPayload, CurveBackground, EffectStage, EntryId, Layer, MAX_ANGLE, MIN_ANGLE,
    ModuleDescriptor, NumberStyle, ORIENTATION_EFFECT, Orientation, ParameterDescriptor,
    ParameterKind, RailDecoration, ResetAction,
};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

/// Local presentation state. The controller owns gesture changes and accepted sampled curves;
/// refresh only reads these values, so a recipe refresh cannot reset a selected channel or point.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ControlsUi {
    pub(crate) group_expanded: BTreeMap<String, bool>,
    /// The tab selected in a module whose descriptor declares `layout: tabs`, keyed by module id.
    /// Per-client view state exactly like `group_expanded`: it changes no recipe and is never sent.
    pub(crate) selected_tab: BTreeMap<String, usize>,
    pub(crate) curve_channels: BTreeMap<(String, String), usize>,
    pub(crate) curve_points: BTreeMap<(String, String), usize>,
    pub(crate) curve_edits: BTreeMap<(String, String, usize, usize), String>,
    pub(crate) curve_samples: BTreeMap<(String, String), CurveSamples>,
    pub(crate) color_open: BTreeMap<(String, String), bool>,
    pub(crate) color_channels: BTreeMap<(String, String, usize), String>,
    pub(crate) color_hex: BTreeMap<(String, String), String>,
    /// Hue and saturation cannot be recovered from gray/black RGB. Keep the picker's fractions
    /// only while its associated RGB still matches the authoritative field.
    pub(crate) picker_hsv: BTreeMap<(String, String), PickerHsv>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PickerHsv {
    pub(crate) rgb: [u8; 3],
    pub(crate) hsv: [f64; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CurveSamples {
    pub(crate) asset: AssetId,
    pub(crate) entry: EntryId,
    pub(crate) source: Value,
    pub(crate) points: Vec<[f32; 2]>,
    pub(crate) version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumberControlStyle {
    Slider,
    Field,
    Stepper,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChoiceControlStyle {
    Segmented,
    Chips,
    Menu,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorControlStyle {
    Fields,
    Picker,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionControlStyle {
    Default,
    Primary,
    Icon,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RailStyle {
    Plain,
    Hue,
    Temperature,
    Tint,
    Gradient(Vec<[u8; 3]>),
}

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

/// How the view arranges a section's top-level groups, derived from the module's declared
/// `layout` exactly like a group's `expanded` is derived from `collapsed`. The view draws the
/// groups of a `Tabs` section as a segmented row, one group visible at a time, instead of the
/// stacked sections a `Stacked` layout draws; that rendering is built elsewhere and this model
/// only carries the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SectionLayout {
    #[default]
    Stacked,
    Tabs {
        /// The index into the section's top-level groups, clamped to the group count.
        selected: usize,
    },
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
    pub(crate) layout: SectionLayout,
    /// A word for the section's own state, shown in its band while expanded: Draft while the
    /// module's canvas draft is open.
    pub(crate) status: Option<String>,
    pub(crate) version: u64,
    pub(crate) enabled: bool,
    /// Why editing is disabled, in the words the status bar would use.
    pub(crate) disabled_reason: Option<String>,
    /// The inputs this section was derived from.
    digest: u64,
}

impl SectionModel {
    /// Every picker this section holds, at any depth. A module declares at most one, so this is
    /// nought or one entry; it walks the tree rather than assuming where the module put it.
    pub(crate) fn pickers(&self) -> Vec<&PickerControl> {
        fn walk<'a>(controls: &'a [ControlModel], found: &mut Vec<&'a PickerControl>) {
            for control in controls {
                match control {
                    ControlModel::Picker(picker) => found.push(picker),
                    ControlModel::Group(group) => walk(&group.controls, found),
                    _ => {}
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.controls, &mut found);
        found
    }
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
    pub(crate) soft_min: f64,
    pub(crate) soft_max: f64,
    /// The rail's increment: the parameter's declared step, else [`generic_step`] over the range.
    pub(crate) step: f64,
    pub(crate) fine_step: f64,
    pub(crate) style: NumberControlStyle,
    pub(crate) rail: RailStyle,
    /// How many decimals the value is shown with, and the precision a drag is quantized to.
    pub(crate) decimals: usize,
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
    pub(crate) style: ChoiceControlStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToggleControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) label: String,
    pub(crate) on: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColorControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) ids: [String; 3],
    pub(crate) label: String,
    pub(crate) channels: [String; 3],
    /// The whole field's text, so one channel edit keeps the other two as typed.
    pub(crate) text: String,
    pub(crate) invalid: Option<String>,
    pub(crate) style: ColorControlStyle,
    pub(crate) rgb: [u8; 3],
    pub(crate) picker_hsv: Option<[f64; 3]>,
    pub(crate) picker_open: bool,
    pub(crate) dragging: bool,
    pub(crate) hex_edit: ValueEdit,
    pub(crate) channel_edits: [ValueEdit; 3],
    pub(crate) version: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CurveChannelModel {
    pub(crate) parameter: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CurvePointRowModel {
    pub(crate) display: [String; 2],
    pub(crate) edit: [ValueEdit; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CurveControl {
    pub(crate) id: (String, String),
    pub(crate) action: String,
    pub(crate) label: String,
    pub(crate) channels: Vec<CurveChannelModel>,
    pub(crate) sample_query: String,
    pub(crate) background: bool,
    pub(crate) selected_channel: usize,
    pub(crate) selected_point: Option<usize>,
    pub(crate) points: Vec<[f32; 2]>,
    pub(crate) sampled: Vec<[f32; 2]>,
    pub(crate) identity: bool,
    pub(crate) point_rows: Vec<CurvePointRowModel>,
    pub(crate) dragging: bool,
    pub(crate) version: u64,
}

pub(crate) fn group_key(module_id: &str, path: &[usize]) -> String {
    let mut key = format!("{module_id}/");
    for (index, part) in path.iter().enumerate() {
        if index > 0 {
            key.push('.');
        }
        key.push_str(&part.to_string());
    }
    key
}

fn choice_style(style: ChoiceStyle, options: usize) -> ChoiceControlStyle {
    match style {
        ChoiceStyle::Automatic if options <= 4 => ChoiceControlStyle::Segmented,
        ChoiceStyle::Segmented => ChoiceControlStyle::Segmented,
        ChoiceStyle::Automatic | ChoiceStyle::Chips => ChoiceControlStyle::Chips,
        ChoiceStyle::Menu => ChoiceControlStyle::Menu,
    }
}

/// Whether every field of one sub-group is still at its declared default.
///
/// It is derived, not declared: no module names these words and none can. A group is `Original`
/// while every one of its value controls shows its parameter's declared default and `Custom` as
/// soon as one does not, which for a field-patch action is exactly "the displayed entry's layer
/// holds nothing for this group", because a patch action's fields mirror that one layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupState {
    Original,
    Custom,
}

impl GroupState {
    /// The caption the sub-group header shows.
    pub(crate) fn caption(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Custom => "Custom",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupControl {
    pub(crate) label: String,
    pub(crate) reset: Option<ResetRef>,
    /// The group's position inside its module's controls, so a reset names it without a search.
    pub(crate) path: Vec<usize>,
    pub(crate) expanded: bool,
    pub(crate) controls: Vec<ControlModel>,
    /// Original or Custom, for a group whose value controls all belong to field-patch actions.
    /// Every other action's fields are request inputs rather than a mirror of a stored layer, so
    /// "original" would mean nothing there and no caption is shown.
    pub(crate) state: Option<GroupState>,
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
    pub(crate) style: ActionControlStyle,
    pub(crate) icon: Option<String>,
}

/// The declaring module's canvas pick, as a button in that module's own panel. It carries no
/// action: clicking it enters the module's canvas mode through `workspace.set`, and clicking it
/// again returns to the pointer, so a pick is never a mode the panel cannot leave.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickerControl {
    /// The module whose canvas mode this button selects.
    pub(crate) module_id: String,
    /// The control's own label, as the module declares it.
    pub(crate) label: String,
    /// The canvas mode's declared title, for the tooltip.
    pub(crate) title: String,
    /// The mode's declared letter, shown beside the title in the tooltip.
    pub(crate) shortcut: Option<String>,
    /// This module's canvas mode is the active one.
    pub(crate) selected: bool,
    /// The mode a click selects: this module's own, or the pointer when this one is already
    /// active, so the mode is always leavable from the button that entered it. The rule is here
    /// rather than in the view, which only publishes the message this names.
    pub(crate) target: String,
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlModel {
    Slider(SliderControl),
    Toggle(ToggleControl),
    Enum(EnumControl),
    Color(ColorControl),
    Curve(CurveControl),
    Group(GroupControl),
    Action(ActionControl),
    Picker(PickerControl),
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

/// The angle's rail while a crop draft is open: the angle's range, the draft's angle on it and
/// the step a drag moves in. The rail's gesture is live for the whole draft, so its handle reads
/// accent while the draft is open, as the crop reference draws it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AngleRailModel {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) value: f64,
    pub(crate) step: f64,
    pub(crate) live: bool,
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
    /// The ratio is locked: the lock reads selected.
    pub(crate) locked: bool,
    pub(crate) can_swap: bool,
    pub(crate) angle: String,
    pub(crate) angle_id: String,
    /// The crop action and its angle parameter, which name the angle field for editing.
    pub(crate) angle_action: String,
    pub(crate) angle_parameter: String,
    /// The angle's box is open for typing; otherwise it shows the angle with its unit.
    pub(crate) angle_editing: bool,
    /// The angle's rail, while a draft is open.
    pub(crate) angle_rail: Option<AngleRailModel>,
    pub(crate) guide: bool,
    /// How far one nudge button moves the angle, in degrees.
    pub(crate) nudge: f64,
    /// The draft's own numbers, so what is on screen is observable without a debugger: each a
    /// name and its value.
    pub(crate) readout: Vec<(String, String)>,
    /// The mode's declared letter, shown on the idle Crop button.
    pub(crate) shortcut: Option<String>,
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
    let layout = section_layout(module, inputs);
    let digest = digest(module, inputs, expanded, enabled, active, layout);
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
        // The reset is declared, so it is always drawn: a section that cannot edit (a historical
        // preview, a request in flight, a missing provider) dims it with the rest of its controls
        // rather than dropping it, because a header that loses its icon changes height and every
        // control under it moves on each commit round trip. The disabled header offers no press.
        reset: ResetRef::of(module.reset.as_ref()),
        controls,
        layout,
        status: (inputs.draft.is_some() && owns_mode(module, inputs)).then(|| "Draft".to_owned()),
        version: previous.map(|previous| previous.version + 1).unwrap_or(1),
        enabled,
        disabled_reason,
        digest,
    }
}

/// Sections start expanded except developer ones and those whose descriptor declares `collapsed`,
/// and the section whose canvas mode is drafting is held open until the draft ends.
fn expanded(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> bool {
    if inputs.draft.is_some() && owns_mode(module, inputs) {
        return true;
    }
    inputs
        .expanded
        .get(&module.id)
        .copied()
        .unwrap_or(!(module.developer || module.collapsed))
}

/// A tabbed section's selected tab, per client and keyed by module id exactly like a group's
/// expansion is keyed by its path: 0 unless a client chose otherwise, clamped to the section's
/// top-level group count so a stale selection from a differently shaped descriptor cannot point
/// past the end.
fn section_layout(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> SectionLayout {
    if module.layout != lightwell_core::ModuleLayout::Tabs {
        return SectionLayout::Stacked;
    }
    let groups = module.controls.len();
    let selected = inputs
        .control_ui
        .selected_tab
        .get(&module.id)
        .copied()
        .unwrap_or(0);
    SectionLayout::Tabs {
        selected: if groups == 0 {
            0
        } else {
            selected.min(groups - 1)
        },
    }
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
    layout: SectionLayout,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    module.id.hash(&mut hasher);
    format!("{:?}", module.availability).hash(&mut hasher);
    (expanded, enabled, active, inputs.developer).hash(&mut hasher);
    match layout {
        SectionLayout::Stacked => 0u8.hash(&mut hasher),
        SectionLayout::Tabs { selected } => (1u8, selected).hash(&mut hasher),
    }
    // Sampled curves may depend on the query's entry context even when their point fields are
    // unchanged. Other modules retain their section version across an unrelated entry switch.
    if contains_curve(&module.controls) {
        inputs.display_entry.hash(&mut hasher);
        inputs.state.map(|state| &state.asset.id).hash(&mut hasher);
    }
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
        for ((curve_action, first_parameter), selected) in &inputs.control_ui.curve_channels {
            if curve_action == &action.id {
                (first_parameter, selected).hash(&mut hasher);
            }
        }
        for ((curve_action, first_parameter), selected) in &inputs.control_ui.curve_points {
            if curve_action == &action.id {
                (first_parameter, selected).hash(&mut hasher);
            }
        }
        for ((sample_action, parameter), samples) in &inputs.control_ui.curve_samples {
            if sample_action == &action.id {
                parameter.hash(&mut hasher);
                samples.version.hash(&mut hasher);
            }
        }
        for ((edit_action, parameter, point, axis), text) in &inputs.control_ui.curve_edits {
            if edit_action == &action.id {
                (parameter, point, axis, text).hash(&mut hasher);
            }
        }
        for ((open_action, parameter), open) in &inputs.control_ui.color_open {
            if open_action == &action.id {
                (parameter, open).hash(&mut hasher);
            }
        }
        for ((hex_action, parameter), text) in &inputs.control_ui.color_hex {
            if hex_action == &action.id {
                (parameter, text).hash(&mut hasher);
            }
        }
        for ((edit_action, parameter, channel), text) in &inputs.control_ui.color_channels {
            if edit_action == &action.id {
                (parameter, channel, text).hash(&mut hasher);
            }
        }
        for ((picker_action, parameter), picker) in &inputs.control_ui.picker_hsv {
            if picker_action == &action.id {
                (parameter, picker.rgb).hash(&mut hasher);
                for fraction in picker.hsv {
                    fraction.to_bits().hash(&mut hasher);
                }
            }
        }
    }
    for (key, value) in &inputs.control_ui.group_expanded {
        if key
            .strip_prefix(&module.id)
            .is_some_and(|rest| rest.starts_with('/'))
        {
            (key, value).hash(&mut hasher);
        }
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
    // This module's picker reads selected while its own canvas mode is active, so entering and
    // leaving that mode re-derives this section and nothing else.
    owns_mode(module, inputs).hash(&mut hasher);
    if owns_mode(module, inputs)
        || matches!(module.canvas, Some(CanvasInteraction::CropFrame { .. }))
    {
        draft_digest(inputs).hash(&mut hasher);
    }
    hasher.finish()
}

fn contains_curve(controls: &[Control]) -> bool {
    controls.iter().any(|control| match control {
        Control::Curve { .. } => true,
        Control::Group { controls, .. } => contains_curve(controls),
        _ => false,
    })
}

/// Everything the crop section shows, as one string. The draft is transient state, so a section
/// that owns the canvas mode follows it.
fn draft_digest(inputs: &Inputs<'_>) -> String {
    match inputs.draft {
        Some(draft) => format!(
            "{}|{}|{}|{:?}|{}|{}|{}|{}",
            draft.summary(),
            draft.preset,
            inputs.crop_angle,
            inputs.editing,
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
            collapsed,
        } => {
            let controls: Vec<ControlModel> = controls
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let mut child_path = path.to_vec();
                    child_path.push(index);
                    control_model(module, child, inputs, enabled, &child_path)
                })
                .collect();
            ControlModel::Group(GroupControl {
                label: label.to_owned(),
                reset: ResetRef::of(reset),
                path: path.to_vec(),
                expanded: inputs
                    .control_ui
                    .group_expanded
                    .get(&group_key(&module.id, path))
                    .copied()
                    .unwrap_or(!collapsed),
                state: group_state(&controls, inputs),
                controls,
            })
        }
        Rendered::Number {
            action,
            parameter,
            label,
            style,
            rail,
        } => {
            let mut model = value_model(module, inputs, action, parameter, label);
            if let ControlModel::Slider(slider) = &mut model {
                slider.style = match style {
                    NumberStyle::Slider => NumberControlStyle::Slider,
                    NumberStyle::Field => NumberControlStyle::Field,
                    NumberStyle::Stepper => NumberControlStyle::Stepper,
                };
                slider.rail = match rail.unwrap_or(&RailDecoration::Plain) {
                    RailDecoration::Plain => RailStyle::Plain,
                    RailDecoration::Hue => RailStyle::Hue,
                    RailDecoration::Temperature => RailStyle::Temperature,
                    RailDecoration::Tint => RailStyle::Tint,
                    RailDecoration::Gradient { stops } => RailStyle::Gradient(stops.clone()),
                };
            }
            model
        }
        Rendered::Toggle {
            action,
            parameter,
            label,
        }
        | Rendered::Choice {
            action,
            parameter,
            label,
            ..
        }
        | Rendered::Color {
            action,
            parameter,
            label,
            ..
        } => {
            let mut model = value_model(module, inputs, action, parameter, label);
            if let Rendered::Choice { style, .. } = classify(control)
                && let ControlModel::Enum(choice) = &mut model
            {
                choice.style = choice_style(style, choice.options.len());
                choice.segmented = choice.style == ChoiceControlStyle::Segmented;
            }
            if let Rendered::Color { style, .. } = classify(control)
                && let ControlModel::Color(color) = &mut model
            {
                color.style = match style {
                    ColorStyle::Fields => ColorControlStyle::Fields,
                    ColorStyle::Picker => ColorControlStyle::Picker,
                };
            }
            model
        }
        Rendered::Curve {
            action,
            channels,
            label,
            sample_query,
            background,
        } => curve_model(inputs, action, channels, label, sample_query, background),
        Rendered::Action {
            action,
            label,
            preset,
            style,
            icon,
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
                style: match style {
                    ActionStyle::Default => ActionControlStyle::Default,
                    ActionStyle::Primary => ActionControlStyle::Primary,
                    ActionStyle::Icon => ActionControlStyle::Icon,
                },
                icon: icon.map(str::to_owned),
            })
        }
        // The picker reads its mode's name and letter from the same canvas declaration the keymap
        // binds, so the panel and the keyboard always agree about what the mode is called.
        Rendered::Picker { label } => ControlModel::Picker(PickerControl {
            module_id: module.id.clone(),
            label: label.to_owned(),
            title: module
                .canvas
                .as_ref()
                .map(CanvasInteraction::title)
                .unwrap_or(label)
                .to_owned(),
            shortcut: module
                .canvas
                .as_ref()
                .and_then(CanvasInteraction::shortcut)
                .map(str::to_owned),
            selected: owns_mode(module, inputs),
            target: if owns_mode(module, inputs) {
                lightwell_core::POINTER_MODE.to_owned()
            } else {
                module.id.clone()
            },
            enabled,
        }),
        Rendered::Unsupported(kind) => ControlModel::Unsupported(unsupported_label(&kind)),
    }
}

/// Whether one sub-group is still at its declared defaults, when that question has an answer.
///
/// Only a group whose value controls all belong to field-patch actions gets one: those fields
/// mirror the module's one stored layer, seeded from the displayed entry's reported values, so
/// "all defaults" is the same statement as "that layer holds nothing for this group". A group of
/// request inputs, a group with no value control at all and a mixed group get no caption.
fn group_state(controls: &[ControlModel], inputs: &Inputs<'_>) -> Option<GroupState> {
    let mut values = 0usize;
    let mut custom = false;
    fn field(
        action: &str,
        parameter: &str,
        inputs: &Inputs<'_>,
        values: &mut usize,
        custom: &mut bool,
    ) -> bool {
        let Some(declared) =
            declared_action(inputs.modules, action).filter(|declared| declared.patch)
        else {
            return false;
        };
        let Some(parameter_desc) = declared.parameter(parameter) else {
            return false;
        };
        *values += 1;
        *custom |= inputs.fields.get(action, parameter)
            != Some(crate::app::fields::seed_text(parameter_desc).as_str());
        true
    }
    fn walk(
        controls: &[ControlModel],
        inputs: &Inputs<'_>,
        values: &mut usize,
        custom: &mut bool,
    ) -> bool {
        for control in controls {
            match control {
                ControlModel::Slider(slider) => {
                    if !field(&slider.action, &slider.parameter, inputs, values, custom) {
                        return false;
                    }
                }
                ControlModel::Toggle(toggle) => {
                    if !field(&toggle.action, &toggle.parameter, inputs, values, custom) {
                        return false;
                    }
                }
                ControlModel::Enum(choice) => {
                    if !field(&choice.action, &choice.parameter, inputs, values, custom) {
                        return false;
                    }
                }
                ControlModel::Color(color) => {
                    if !field(&color.action, &color.parameter, inputs, values, custom) {
                        return false;
                    }
                }
                ControlModel::Curve(curve) => {
                    for channel in &curve.channels {
                        if !field(&curve.action, &channel.parameter, inputs, values, custom) {
                            return false;
                        }
                    }
                }
                ControlModel::Group(group) => {
                    if !walk(&group.controls, inputs, values, custom) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    }
    if !walk(controls, inputs, &mut values, &mut custom) || values == 0 {
        return None;
    }
    Some(if custom {
        GroupState::Custom
    } else {
        GroupState::Original
    })
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
        ParameterKind::Color => {
            let rgb = parse_field(declared, text)
                .ok()
                .and_then(|value| value.as_array().cloned())
                .and_then(|values| {
                    Some([
                        values.first()?.as_u64()? as u8,
                        values.get(1)?.as_u64()? as u8,
                        values.get(2)?.as_u64()? as u8,
                    ])
                })
                .unwrap_or([0, 0, 0]);
            let picker_hsv = inputs
                .control_ui
                .picker_hsv
                .get(&(action.to_owned(), parameter.to_owned()))
                .filter(|picker| picker.rgb == rgb)
                .map(|picker| picker.hsv);
            let dragging = inputs
                .dragging
                .is_some_and(|(a, p)| a == action && p == parameter);
            ControlModel::Color(ColorControl {
                action: action.to_owned(),
                parameter: parameter.to_owned(),
                ids: [0, 1, 2].map(|index| {
                    field_id(action, parameter, Some(crate::app::fields::CHANNELS[index]))
                }),
                label: labelled(label, declared),
                channels: [0, 1, 2].map(|index| channel_text(text, index).to_owned()),
                text: text.to_owned(),
                invalid,
                style: ColorControlStyle::Fields,
                rgb,
                picker_hsv,
                picker_open: inputs
                    .control_ui
                    .color_open
                    .get(&(action.to_owned(), parameter.to_owned()))
                    .copied()
                    .unwrap_or(false),
                dragging,
                hex_edit: inputs
                    .control_ui
                    .color_hex
                    .get(&(action.to_owned(), parameter.to_owned()))
                    .map(|text| ValueEdit::Typing(text.clone()))
                    .unwrap_or_default(),
                channel_edits: [0, 1, 2].map(|index| {
                    inputs
                        .control_ui
                        .color_channels
                        .get(&(action.to_owned(), parameter.to_owned(), index))
                        .map(|text| ValueEdit::Typing(text.clone()))
                        .unwrap_or_default()
                }),
                version: {
                    let mut hasher = DefaultHasher::new();
                    text.hash(&mut hasher);
                    dragging.hash(&mut hasher);
                    for fraction in picker_hsv.unwrap_or_default() {
                        fraction.to_bits().hash(&mut hasher);
                    }
                    hasher.finish()
                },
            })
        }
        ParameterKind::Enum { options } => ControlModel::Enum(EnumControl {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            id: field_id(action, parameter, None),
            label: labelled(label, declared),
            options: options.clone(),
            selected: options.iter().position(|option| option == text.trim()),
            segmented: options.len() <= 4,
            style: choice_style(ChoiceStyle::Automatic, options.len()),
        }),
        ParameterKind::Boolean => ControlModel::Toggle(ToggleControl {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            label: labelled(label, declared),
            on: parse_field(declared, text)
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        }),
        ParameterKind::Curve { .. } => ControlModel::Unsupported(format!(
            "curve parameter {parameter} of action {action} needs a curve control"
        )),
    }
}

/// The generic increment for a number parameter that declares no step: a fraction of its range,
/// rounded to a power of ten. It is the model's decision, not the view's, because the same number
/// decides the rail's step, the decimals the field shows and the precision a drag is quantized to,
/// and those three must agree.
pub(crate) fn generic_step(min: f64, max: f64) -> f64 {
    let span = (max - min).abs();
    if !span.is_finite() || span <= 0.0 {
        return 0.01;
    }
    10f64.powf((span / 200.0).log10().round())
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
    // An integer parameter steps by one; every other one takes the step it declares, and falls
    // back to the generic one over its range.
    let step = if integer {
        1.0
    } else {
        declared
            .step
            .filter(|step| step.is_finite() && *step > 0.0)
            .unwrap_or_else(|| generic_step(min, max))
    };
    SliderControl {
        action: action.to_owned(),
        parameter: parameter.to_owned(),
        id: field_id(action, parameter, None),
        // The value carries the unit, so the label does not repeat it.
        label: label.to_owned(),
        unit: declared.unit.clone(),
        min,
        max,
        soft_min: declared.soft_min.unwrap_or(min),
        soft_max: declared.soft_max.unwrap_or(max),
        step,
        fine_step: declared.fine_step.unwrap_or(step / 10.0),
        style: NumberControlStyle::Slider,
        rail: RailStyle::Plain,
        decimals: decimals_for(declared),
        zero: declared.zero.unwrap_or(0.0_f64.clamp(min, max)),
        value,
        display: if invalid.is_some() {
            text.to_owned()
        } else {
            format_number(declared, value)
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

fn curve_model(
    inputs: &Inputs<'_>,
    action: &str,
    channels: &[lightwell_core::CurveChannel],
    label: &str,
    sample_query: &str,
    background: CurveBackground,
) -> ControlModel {
    let id = (
        action.to_owned(),
        channels
            .first()
            .map(|channel| channel.parameter.clone())
            .unwrap_or_default(),
    );
    let selected_channel = inputs
        .control_ui
        .curve_channels
        .get(&id)
        .copied()
        .unwrap_or(0)
        .min(channels.len().saturating_sub(1));
    let Some(channel) = channels.get(selected_channel) else {
        return ControlModel::Unsupported(format!(
            "curve control of action {action} has no channel"
        ));
    };
    let parameter = &channel.parameter;
    let text = inputs.fields.get(action, parameter).unwrap_or_default();
    let declared = inputs
        .modules
        .iter()
        .find_map(|module| module.action(action))
        .and_then(|declared| declared.parameter(parameter));
    let precision = declared
        .and_then(|parameter| parameter.precision)
        .unwrap_or(3) as usize;
    let parsed = declared.and_then(|declared| parse_field(declared, text).ok());
    let points = parsed
        .as_ref()
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .filter_map(|point| {
                    let pair = point.as_array()?;
                    Some([
                        pair.first()?.as_f64()? as f32,
                        pair.get(1)?.as_f64()? as f32,
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected_point = inputs
        .control_ui
        .curve_points
        .get(&id)
        .copied()
        .filter(|index| *index < points.len());
    let point_rows = points
        .iter()
        .enumerate()
        .map(|(index, point)| CurvePointRowModel {
            display: [0, 1].map(|axis| {
                // Plot coordinates use f32; field text retains the authoritative f64 value.
                let value = parsed
                    .as_ref()
                    .and_then(|value| value.get(index))
                    .and_then(|value| value.get(axis))
                    .and_then(Value::as_f64)
                    .unwrap_or(point[axis] as f64);
                let formatted = format!("{value:.precision$}");
                if precision == 0 {
                    formatted
                } else {
                    formatted
                        .trim_end_matches('0')
                        .trim_end_matches('.')
                        .to_owned()
                }
            }),
            edit: [0, 1].map(|axis| {
                inputs
                    .control_ui
                    .curve_edits
                    .get(&(action.to_owned(), parameter.to_owned(), index, axis))
                    .map(|text| ValueEdit::Typing(text.clone()))
                    .unwrap_or_default()
            }),
        })
        .collect();
    let samples = inputs
        .control_ui
        .curve_samples
        .get(&(action.to_owned(), parameter.to_owned()))
        .filter(|samples| {
            parsed.as_ref() == Some(&samples.source)
                && inputs.display_entry == Some(&samples.entry)
                && inputs
                    .state
                    .is_some_and(|state| state.asset.id == samples.asset)
        });
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    selected_channel.hash(&mut hasher);
    selected_point.hash(&mut hasher);
    samples.map(|samples| samples.version).hash(&mut hasher);
    let dragging = inputs
        .dragging
        .is_some_and(|(a, p)| a == action && p == parameter);
    dragging.hash(&mut hasher);
    ControlModel::Curve(CurveControl {
        id,
        action: action.to_owned(),
        label: label.to_owned(),
        channels: channels
            .iter()
            .map(|channel| CurveChannelModel {
                parameter: channel.parameter.clone(),
                label: channel.label.clone(),
            })
            .collect(),
        sample_query: sample_query.to_owned(),
        background: matches!(background, CurveBackground::Histogram),
        selected_channel,
        selected_point,
        identity: points.iter().all(|point| point[0] == point[1]),
        points,
        sampled: samples
            .map(|samples| samples.points.clone())
            .unwrap_or_default(),
        point_rows,
        dragging,
        version: hasher.finish(),
    })
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
        angle_action: frame.action.to_owned(),
        angle_parameter: frame.angle.to_owned(),
        angle_editing: inputs
            .editing
            .is_some_and(|(action, parameter)| action == frame.action && parameter == frame.angle),
        guide: inputs.crop_guide,
        nudge: crate::app::crop::ANGLE_STEP,
        shortcut: frame
            .module
            .canvas
            .as_ref()
            .and_then(|canvas| canvas.shortcut())
            .map(str::to_owned),
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
        locked: draft.aspect.ratio().is_some(),
        can_swap: enabled && draft.aspect.ratio().is_some(),
        can_apply: enabled && !draft.conflicted,
        can_reapply: !inputs.busy,
        readout: readout(draft, frame.action),
        angle_rail: Some(AngleRailModel {
            min: MIN_ANGLE,
            max: MAX_ANGLE,
            value: draft.stage.angle,
            step: crate::app::crop::ANGLE_RAIL_STEP,
            live: true,
        }),
        ..base
    }
}

/// The draft's own numbers, in the order the panel prints them: the input stage, the rectangle in
/// the rotated stage's box, the whole-pixel output, and the request Apply commits.
fn readout(draft: &CropDraft, action: &str) -> Vec<(String, String)> {
    let (box_width, box_height) = draft.stage.bounding_box();
    let stage = if (box_width, box_height)
        == (f64::from(draft.stage.width), f64::from(draft.stage.height))
    {
        format!("{} × {}", draft.stage.width, draft.stage.height)
    } else {
        format!(
            "{} × {} · box {:.0} × {:.0}",
            draft.stage.width, draft.stage.height, box_width, box_height
        )
    };
    let output = match draft.output() {
        Ok(rect) => format!("{} × {}", rect.width, rect.height),
        Err(error) => error.detail.clone(),
    };
    let layer = match draft.layer {
        Some(_) => format!("layer {}", draft.layer_index + 1),
        None => "new layer".to_owned(),
    };
    vec![
        ("Input stage".to_owned(), stage),
        (
            "Rectangle".to_owned(),
            format!(
                "{:.0}, {:.0} · {:.0} × {:.0}",
                draft.rect.x, draft.rect.y, draft.rect.width, draft.rect.height
            ),
        ),
        ("Output".to_owned(), output),
        ("Commits".to_owned(), format!("edit.{action} · {layer}")),
    ]
}

// ---- descriptor mapping ------------------------------------------------------------------------

/// What the desktop makes of one declared control. A kind this build cannot draw keeps its name on
/// screen rather than disappearing from the panel.
pub(crate) enum Rendered<'a> {
    Group {
        label: &'a str,
        controls: &'a [Control],
        reset: Option<&'a ResetAction>,
        collapsed: bool,
    },
    Number {
        action: &'a str,
        parameter: &'a str,
        label: &'a str,
        style: NumberStyle,
        rail: Option<&'a RailDecoration>,
    },
    Toggle {
        action: &'a str,
        parameter: &'a str,
        label: &'a str,
    },
    Choice {
        action: &'a str,
        parameter: &'a str,
        label: &'a str,
        style: ChoiceStyle,
    },
    Color {
        action: &'a str,
        parameter: &'a str,
        label: &'a str,
        style: ColorStyle,
    },
    Curve {
        action: &'a str,
        channels: &'a [lightwell_core::CurveChannel],
        label: &'a str,
        sample_query: &'a str,
        background: CurveBackground,
    },
    Action {
        action: &'a str,
        label: &'a str,
        preset: &'a Map<String, Value>,
        style: ActionStyle,
        icon: Option<&'a str>,
    },
    /// The declaring module's own canvas pick, offered in its panel.
    Picker {
        label: &'a str,
    },
    Unsupported(String),
}

pub(crate) fn classify(control: &Control) -> Rendered<'_> {
    match control {
        Control::Group {
            label,
            controls,
            reset,
            collapsed,
        } => Rendered::Group {
            label,
            controls,
            reset: reset.as_ref(),
            collapsed: *collapsed,
        },
        Control::Number {
            action,
            parameter,
            label,
            style,
            rail,
        } => Rendered::Number {
            action,
            parameter,
            label,
            style: *style,
            rail: rail.as_ref(),
        },
        Control::Toggle {
            action,
            parameter,
            label,
        } => Rendered::Toggle {
            action,
            parameter,
            label,
        },
        Control::Choice {
            action,
            parameter,
            label,
            style,
        } => Rendered::Choice {
            action,
            parameter,
            label,
            style: *style,
        },
        Control::Curve {
            action,
            channels,
            label,
            sample_query,
            background,
        } => Rendered::Curve {
            action,
            channels,
            label,
            sample_query,
            background: *background,
        },
        Control::Color {
            action,
            parameter,
            label,
            style,
        } => Rendered::Color {
            action,
            parameter,
            label,
            style: *style,
        },
        Control::Action {
            action,
            label,
            preset,
            style,
            icon,
        } => Rendered::Action {
            action,
            label,
            preset,
            style: *style,
            icon: icon.as_deref(),
        },
        Control::Picker { label } => Rendered::Picker { label },
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

/// This parameter is the only one its action declares, so one field is already the whole request.
///
/// A control of such an action drafts for the same reason a patch action's control does: the one
/// value the gesture moves is a complete, valid request on its own, which is what `draft.set`
/// validates, `draft_recipe` plans and `draft.commit` applies. An action with a second parameter
/// cannot: one field of it is not a request, so its slider keeps the older behaviour of changing
/// the text and submitting the whole action once on release.
pub(crate) fn drafts_alone(modules: &[ModuleDescriptor], action: &str, parameter: &str) -> bool {
    declared_action(modules, action).is_some_and(|declared| {
        declared.parameters.len() == 1 && declared.parameters[0].name == parameter
    })
}

/// A slider of this control drafts: `draft.begin`, a gated `draft.set` with a live preview per
/// tick, and one `draft.commit` on release.
pub(crate) fn drafts(modules: &[ModuleDescriptor], action: &str, parameter: &str) -> bool {
    is_patch(modules, action) || drafts_alone(modules, action, parameter)
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
            ..
        }
        | Rendered::Color {
            action: declared,
            parameter: named,
            label,
            ..
        }
        | Rendered::Toggle {
            action: declared,
            parameter: named,
            label,
        }
        | Rendered::Choice {
            action: declared,
            parameter: named,
            label,
            ..
        } if declared == action && named == parameter => Some(label),
        Rendered::Curve {
            action: declared,
            channels,
            label,
            ..
        } if declared == action
            && channels
                .iter()
                .any(|channel| channel.parameter == parameter) =>
        {
            Some(label)
        }
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

/// The first available module that declares a plain canvas point pick: its action and coordinate
/// parameters. Dispatch goes through [`canvas_pick`], which answers for the mode that is actually
/// on screen; this is the tests' way of naming the one module that declares a point pick without
/// hard-coding it.
#[cfg(test)]
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

/// What a click on the photograph does in one canvas mode, as that mode's module declares it.
///
/// A pick belongs to the mode the session is in, never to "whichever module declares one first":
/// several modules declare a canvas pick, and only the one whose canvas is on screen may answer
/// for a click. A crop frame is a different adapter and is not a pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasPick<'a> {
    /// Fill these coordinate parameters of an action with the located content pixel. It commits
    /// nothing: the person submits the action themselves.
    Point {
        action: &'a str,
        x: &'a str,
        y: &'a str,
    },
    /// Run this query at the located content pixel and submit the fields it answers with to the
    /// action once. A refused query commits nothing and its reason is shown.
    Sample {
        query: &'a str,
        x: &'a str,
        y: &'a str,
        action: &'a str,
    },
}

/// The pick the active canvas mode declares, if that mode declares one at all.
pub(crate) fn canvas_pick<'a>(
    modules: &'a [ModuleDescriptor],
    mode: &str,
) -> Option<CanvasPick<'a>> {
    let module = module_of(modules, mode).filter(|module| module.is_available())?;
    match module.canvas.as_ref()? {
        CanvasInteraction::PointPick { action, x, y, .. } => Some(CanvasPick::Point {
            action: action.as_str(),
            x: x.as_str(),
            y: y.as_str(),
        }),
        CanvasInteraction::SampleApply {
            query,
            x,
            y,
            action,
            ..
        } => Some(CanvasPick::Sample {
            query: query.as_str(),
            x: x.as_str(),
            y: y.as_str(),
            action: action.as_str(),
        }),
        CanvasInteraction::CropFrame { .. } => None,
    }
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
                ..
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
    fn a_canvas_mode_routes_only_to_its_own_declared_pick() {
        let modules: Vec<_> = lightwell_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect();
        // The RAW sensor picker and the pixel proof both declare a plain point pick, and each
        // answers only for its own mode.
        assert_eq!(
            canvas_pick(&modules, "lightwell.raw"),
            Some(CanvasPick::Point {
                action: "pick-raw-neutral",
                x: "x",
                y: "y"
            })
        );
        assert_eq!(
            canvas_pick(&modules, "lightwell.pixel"),
            Some(CanvasPick::Point {
                action: "set-pixel",
                x: "x",
                y: "y"
            })
        );
        // Basic's neutral picker is a sample-apply pick: a query first, then one command.
        assert_eq!(
            canvas_pick(&modules, "lightwell.basic"),
            Some(CanvasPick::Sample {
                query: "neutral-sample",
                x: "x",
                y: "y",
                action: "set-basic"
            })
        );
        // A crop frame is a different adapter, and the pointer mode names no module at all.
        assert!(canvas_pick(&modules, "lightwell.crop").is_none());
        assert!(canvas_pick(&modules, lightwell_core::POINTER_MODE).is_none());
    }

    /// Which generated sliders draft. The rule is about the request, not the module: one field is a
    /// whole request when the action merges it or declares nothing else, and only then.
    #[test]
    fn a_slider_drafts_for_a_patch_field_or_an_actions_only_parameter() {
        let modules: Vec<_> = lightwell_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect();
        // RAW declares one action per field: each one is a complete request on its own.
        for (action, parameter) in [
            ("set-raw-exposure", "ev"),
            ("set-raw-temperature", "kelvin"),
            ("set-raw-tint", "tint"),
        ] {
            assert!(
                drafts_alone(&modules, action, parameter),
                "{action}.{parameter} declares no second parameter"
            );
            assert!(!is_patch(&modules, action), "{action} is not a field patch");
            assert!(drafts(&modules, action, parameter));
        }
        // Basic's fields are a patch: the module merges whichever ones it is sent.
        assert!(is_patch(&modules, "set-basic"));
        assert!(drafts(&modules, "set-basic", "temperature"));
        assert!(
            !drafts_alone(&modules, "set-basic", "temperature"),
            "a patch action declares more than one field; it drafts for the other reason"
        );
        // An action with a second parameter cannot send one field alone, so its slider does not
        // draft: the crop rectangle, the pixel proof's coordinates and colour.
        for (action, parameter) in [("crop", "angle"), ("set-pixel", "x"), ("set-pixel", "y")] {
            assert!(
                !drafts(&modules, action, parameter),
                "{action}.{parameter} is one field of several"
            );
        }
        // A parameter no action declares, and an action no module declares, draft nothing.
        assert!(!drafts(&modules, "set-raw-exposure", "kelvin"));
        assert!(!drafts(&modules, "no-such-action", "ev"));
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
