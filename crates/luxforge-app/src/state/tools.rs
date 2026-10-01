//! The tools panel model: the descriptor-to-control mapping and one section per registered module,
//! derived again after every message.
use crate::{
    crop_draft::{AspectPreset, CropDraft, committed_aspect},
    state::{
        Inputs, MenuTarget,
        capabilities::{self, CapabilityModel, TaskControl},
        control_tree::{Walk, walk},
        fields::{action_params, channel_text, field_id, labelled, parse_field, undeclared_label},
        number::NumberSpec,
        palette::PaletteAction,
        presets::{PresetsModel, presets_model},
    },
};
use luxforge_core::{
    ActionDescriptor, ActionStyle, AssetId, CanvasInteraction, ChoiceStyle, ColorStyle, Control,
    CropPayload, CropStage, CurveBackground, EditorState, EffectStage, EntryId, LayerDescription,
    MaskId, ModuleDescriptor, NumberStyle, ParameterDescriptor, ParameterKind, RailDecoration,
    RecipeDescription, ResetAction, SourceTag,
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
    pub(crate) query_choices: BTreeMap<String, super::query_choice::QueryChoiceUi>,
    pub(crate) group_expanded: BTreeMap<String, bool>,
    /// The tab selected in a module whose descriptor declares `layout: tabs`, keyed by module id.
    /// Per-client view state exactly like `group_expanded`: it changes no recipe and is never sent.
    pub(crate) selected_tab: BTreeMap<String, usize>,
    /// Each generated control's own local state, by the control's key. Only a control that holds
    /// some has an entry.
    controls: BTreeMap<ControlKey, ControlUi>,
}

/// A generated control's identity in [`ControlsUi`]: its action and the parameter it edits. A curve
/// is keyed by its first channel's parameter, as [`CurveControl::id`] is, so every channel of one
/// curve shares one entry. A parameter has one kind, so a key names one kind of control.
pub(crate) type ControlKey = (String, String);

/// One control's local state, by the kind of control that holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlUi {
    Curve(CurveUi),
    Color(ColorUi),
}

/// A curve control's local state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CurveUi {
    /// The channel shown, an index into the declared channels.
    pub(crate) channel: usize,
    /// The point selected on the shown channel.
    pub(crate) point: Option<usize>,
    /// A point row's text as typed, by channel parameter, point index and axis.
    pub(crate) edits: BTreeMap<(String, usize, usize), String>,
    /// The accepted sampled curve of each channel, by channel parameter.
    pub(crate) samples: BTreeMap<String, CurveSamples>,
    /// The Points disclosure is open. View state, closed by default, shared by the global and
    /// masked targets and kept while the window is open: a sample refresh or a photo change
    /// leaves it as it is.
    pub(crate) points_open: bool,
}

/// A colour control's local state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ColorUi {
    /// The picker is open.
    pub(crate) open: bool,
    /// The R, G and B boxes' text as typed.
    pub(crate) channels: [Option<String>; 3],
    /// The hex box's text as typed.
    pub(crate) hex: Option<String>,
    /// Hue and saturation cannot be recovered from gray/black RGB. Keep the picker's fractions
    /// only while its associated RGB still matches the authoritative field.
    pub(crate) hsv: Option<PickerHsv>,
}

impl ControlsUi {
    /// The curve keyed `key`, when it holds local state.
    pub(crate) fn curve(&self, key: &ControlKey) -> Option<&CurveUi> {
        match self.controls.get(key)? {
            ControlUi::Curve(curve) => Some(curve),
            ControlUi::Color(_) => None,
        }
    }

    /// The curve keyed `key`'s local state, created empty on first use.
    pub(crate) fn curve_mut(&mut self, key: ControlKey) -> &mut CurveUi {
        let state = self
            .controls
            .entry(key)
            .or_insert_with(|| ControlUi::Curve(CurveUi::default()));
        if !matches!(state, ControlUi::Curve(_)) {
            *state = ControlUi::Curve(CurveUi::default());
        }
        match state {
            ControlUi::Curve(curve) => curve,
            ControlUi::Color(_) => unreachable!("the entry was made a curve above"),
        }
    }

    /// The colour field keyed `key`, when it holds local state.
    pub(crate) fn color(&self, key: &ControlKey) -> Option<&ColorUi> {
        match self.controls.get(key)? {
            ControlUi::Color(color) => Some(color),
            ControlUi::Curve(_) => None,
        }
    }

    /// The colour field keyed `key`'s local state, created empty on first use.
    pub(crate) fn color_mut(&mut self, key: ControlKey) -> &mut ColorUi {
        let state = self
            .controls
            .entry(key)
            .or_insert_with(|| ControlUi::Color(ColorUi::default()));
        if !matches!(state, ControlUi::Color(_)) {
            *state = ControlUi::Color(ColorUi::default());
        }
        match state {
            ControlUi::Color(color) => color,
            ControlUi::Curve(_) => unreachable!("the entry was made a colour field above"),
        }
    }

    /// Every control's local state, in key order.
    pub(crate) fn controls(&self) -> impl Iterator<Item = (&ControlKey, &ControlUi)> {
        self.controls.iter()
    }

    /// Drop the accepted samples of one channel of the curve keyed `key`, whose points changed.
    pub(crate) fn forget_curve_samples(&mut self, key: &ControlKey, parameter: &str) {
        if let Some(ControlUi::Curve(curve)) = self.controls.get_mut(key) {
            curve.samples.remove(parameter);
        }
    }

    /// Drop every curve's accepted samples, which describe a displayed entry that is gone.
    pub(crate) fn clear_curve_samples(&mut self) {
        for state in self.controls.values_mut() {
            if let ControlUi::Curve(curve) = state {
                curve.samples.clear();
            }
        }
    }
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
    /// The − and + buttons either side of the value box, or of a rail between them when `rail` is
    /// set: the crop angle's, which a drag moves on the parameter's fine step.
    Stepper {
        rail: bool,
    },
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

/// One registered module's section.
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
    /// The resolved reset of the one group a headerless module's controls consist of, at path
    /// `[0]` ([`headerless_group`]). The panel draws no header for that group, so no
    /// [`GroupControl`] carries it, but a `ResetGroup` naming the group still runs it.
    pub(crate) headerless_reset: Option<ResetRef>,
    /// The module's resources, settings and permissions, above its controls, when it declares
    /// settings, resources or tasks.
    pub(crate) capability: Option<CapabilityModel>,
    pub(crate) controls: Vec<ControlModel>,
    pub(crate) layout: SectionLayout,
    /// A word for the section's own state, shown in its band while expanded: Draft while the
    /// module's canvas draft is open.
    pub(crate) status: Option<String>,
    /// The covered canvas reported by the host, shared by all geometry sections.
    pub(crate) geometry_summary: Option<String>,
    /// The name of the mask this section's controls edit through, drawn as the band's accent scope
    /// chip: set on a maskable module's section while the sections are bound to an open mask, and
    /// `None` everywhere else, so leaving Mask mode drops it.
    pub(crate) scope: Option<String>,
    pub(crate) enabled: bool,
    /// Why editing is disabled, in the words the status bar would use.
    pub(crate) disabled_reason: Option<String>,
}

impl SectionModel {
    /// The preset library this section renders, when its module declares the `presets` control.
    pub(crate) fn presets(&self) -> Option<&PresetsModel> {
        walk(&self.controls).find_map(|control| match control {
            ControlModel::Presets(presets) => Some(presets.as_ref()),
            _ => None,
        })
    }

    /// Every picker this section holds, at any depth. A module declares at most one, so this is
    /// nought or one entry; it walks the tree rather than assuming where the module put it.
    pub(crate) fn pickers(&self) -> Vec<&PickerControl> {
        walk(&self.controls)
            .filter_map(|control| match control {
                ControlModel::Picker(picker) => Some(picker),
                _ => None,
            })
            .collect()
    }

    /// The section draws its controls: it is expanded and its module is available.
    pub(crate) fn shows_controls(&self) -> bool {
        self.expanded && self.unavailable.is_none()
    }

    /// The one top-level group a `layout: tabs` section shows: the selected tab's, or the first
    /// when the selection names none. `None` for a stacked section, or a tabbed one with no group,
    /// which draws its controls stacked.
    pub(crate) fn visible_tab(&self) -> Option<&GroupControl> {
        let SectionLayout::Tabs { selected } = self.layout else {
            return None;
        };
        let mut groups = self.controls.iter().filter_map(|control| match control {
            ControlModel::Group(group) => Some(group),
            _ => None,
        });
        let first = groups.next()?;
        Some(match selected {
            0 => first,
            selected => groups.nth(selected - 1).unwrap_or(first),
        })
    }

    /// Every curve this section has on screen, in the order the panel draws them: none while the
    /// section is collapsed or unavailable; in a tabbed section only the visible tab's; and none
    /// inside a collapsed group. The section already holds only the controls that apply to the
    /// photo's source kind, each resolved to the variant that provides it, so this is the whole of
    /// "which curves are visible". No allocation beyond the walk's one frame per open group.
    pub(crate) fn shown_curves(&self) -> ShownCurves<'_> {
        let controls: &[ControlModel] = if self.shows_controls() {
            &self.controls
        } else {
            &[]
        };
        ShownCurves {
            walk: walk(controls),
            tab: self.visible_tab(),
        }
    }

    /// The reset of the group at `path` in this section's module, as this section resolved it
    /// for the photo's source kind and the bound target: on a RAW photo's global target White
    /// balance's reset is the RAW development's As shot.
    pub(crate) fn group_reset(&self, path: &[usize]) -> Option<&ResetRef> {
        if path == [0] && self.headerless_reset.is_some() {
            return self.headerless_reset.as_ref();
        }
        walk(&self.controls).find_map(|control| match control {
            ControlModel::Group(group) if group.path == path => group.reset.as_ref(),
            _ => None,
        })
    }
}

/// The curves a section has on screen ([`SectionModel::shown_curves`]).
pub(crate) struct ShownCurves<'a> {
    walk: Walk<'a, ControlModel>,
    /// The visible tab of a tabbed section, whose own disclosure does not hide it.
    tab: Option<&'a GroupControl>,
}

impl<'a> Iterator for ShownCurves<'a> {
    type Item = &'a CurveControl;

    fn next(&mut self) -> Option<&'a CurveControl> {
        loop {
            match self.walk.next()? {
                ControlModel::Curve(curve) => return Some(curve),
                ControlModel::Group(group) => {
                    let shown = match self.tab {
                        Some(tab) if self.walk.depth() == 1 => std::ptr::eq(group, tab),
                        _ => group.expanded,
                    };
                    if !shown {
                        self.walk.skip_children();
                    }
                }
                _ => {}
            }
        }
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
    /// A box's text as typed, when some is held for it.
    fn typed(text: Option<&String>) -> Self {
        text.map_or(Self::None, |text| Self::Typing(text.clone()))
    }

    /// The text a field shows: what is being typed, else the formatted value.
    pub(crate) fn text<'a>(&'a self, display: &'a str) -> &'a str {
        match self {
            Self::Typing(text) => text,
            Self::None => display,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) unit: Option<String>,
    /// The range, rail, steps, decimals and zero the parameter declares, read once.
    pub(crate) spec: NumberSpec,
    pub(crate) style: NumberControlStyle,
    pub(crate) rail: RailStyle,
    pub(crate) value: f64,
    /// The formatted value, or the text as typed when it cannot be read.
    pub(crate) display: String,
    pub(crate) edit: ValueEdit,
    pub(crate) dragging: bool,
    /// The declared range, when the text does not satisfy it.
    pub(crate) invalid: Option<String>,
    /// The parameter's declared default, already formatted: what a reset sets the field to.
    pub(crate) default: String,
    /// The action a reset of this field runs instead of that default, when its control declares
    /// one. Such a field's neutral is not a value its text can show (a RAW white balance at As
    /// shot shows the camera's equivalent temperature), so its group reads its layer instead.
    pub(crate) reset: Option<ResetRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnumControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) options: Vec<String>,
    pub(crate) selected: Option<usize>,
    pub(crate) style: ChoiceControlStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToggleControl {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) label: String,
    pub(crate) on: bool,
}

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
    /// Draw the label line above the plot. False for a curve that is the only control of a
    /// [`headerless_group`], whose band already names it.
    pub(crate) label_shown: bool,
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
    /// The Points disclosure is open ([`CurveUi::points_open`]).
    pub(crate) points_open: bool,
    /// The most points the shown channel's kind declares.
    pub(crate) points_max: usize,
    /// [`CURVE_HINT`] when points can be added and removed: the kind fixes no `x` and its point
    /// count may vary.
    pub(crate) hint: Option<String>,
}

/// The line under a curve plot whose points can be added and removed.
pub(crate) const CURVE_HINT: &str = "Click to add a point, or double-click one to remove it";

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

/// A band on one axis: its two edges and, where declared, its two shoulders, each exactly the
/// number field its parameter is. A thumb or grip drags that one field through the slider's own
/// draft path, and the fields are drawn under the band, because a typed value is exact.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RangeControl {
    pub(crate) action: String,
    pub(crate) label: String,
    pub(crate) rail: RailStyle,
    pub(crate) low: SliderControl,
    pub(crate) high: SliderControl,
    pub(crate) low_feather: Option<SliderControl>,
    pub(crate) high_feather: Option<SliderControl>,
}

impl RangeControl {
    /// The band's fields in the order they are laid out, two to a row: the low edge and its
    /// shoulder, then the high edge and its shoulder.
    pub(crate) fn fields(&self) -> impl Iterator<Item = &SliderControl> {
        [
            Some(&self.low),
            self.low_feather.as_ref(),
            Some(&self.high),
            self.high_feather.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    /// Whether this band draws the field of `parameter` of `action` under itself.
    pub(crate) fn draws(&self, action: &str, parameter: &str) -> bool {
        self.fields()
            .any(|field| field.action == action && field.parameter == parameter)
    }
}

/// Whether a range control among `controls` already draws `control`, a number field, under its
/// band, so a list that holds both draws the field once. A declarer may list a band's fields as
/// number controls of their own — the host does, so every field keeps its declared label and an
/// agent reads it as one — and the panel still shows each field exactly once.
pub(crate) fn drawn_by_range(controls: &[ControlModel], control: &ControlModel) -> bool {
    let ControlModel::Slider(field) = control else {
        return false;
    };
    controls.iter().any(|other| {
        matches!(other, ControlModel::Range(range) if range.draws(&field.action, &field.parameter))
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlModel {
    Slider(SliderControl),
    /// A two-thumb band over number fields of one action.
    Range(Box<RangeControl>),
    Toggle(ToggleControl),
    Enum(EnumControl),
    QueryChoice(Box<super::query_choice::QueryChoiceModel>),
    Color(ColorControl),
    Curve(CurveControl),
    Group(GroupControl),
    Action(ActionControl),
    Picker(PickerControl),
    /// A button that runs one of the module's worker tasks through consent and progress, and
    /// offers Apply with its result when the task declares one.
    Task(TaskControl),
    /// A control this build cannot draw keeps its name on screen rather than disappearing.
    Unsupported(String),
    /// The host's crop-frame editor, at the top of the declaring module's section.
    CropFrame(Box<CropSectionModel>),
    /// The host's preset library, where the module declares its `presets` control.
    Presets(Box<PresetsModel>),
}

/// One generated ratio preset button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetChip {
    pub(crate) index: usize,
    pub(crate) label: String,
    pub(crate) chosen: bool,
}

/// The crop draft's own controls, rendered by the host for a declared crop-frame interaction.
///
/// Idle, the same Ratio and Angle controls read the displayed entry's committed crop exactly as a
/// draft opened on it would seed them; a change opens that draft and applies the change to it.
/// While drafting the preset chips appear in the floating bar rather than in the section.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CropSectionModel {
    pub(crate) title: String,
    /// A draft is open.
    pub(crate) drafting: bool,
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
    /// The crop action's declared angle as the generic stepper with its rail: the draft's angle
    /// while drafting, the committed one while idle, and the text as typed while its box is open.
    /// The rail's gesture is live for the whole draft, so its handle reads accent while the draft
    /// is open, as the crop reference draws it. `None` when the action declares no number angle.
    pub(crate) angle: Option<SliderControl>,
    pub(crate) guide: bool,
    /// The draft's own numbers, so what is on screen is observable without a debugger: each a
    /// name and its value.
    pub(crate) readout: Vec<(String, String)>,
    /// Apply can run: the app's one refusal ([`Inputs::apply_refusal`]) has nothing to say.
    pub(crate) can_apply: bool,
    pub(crate) can_reapply: bool,
    pub(crate) enabled: bool,
}

/// The tools panel for these inputs: one section per registered module that applies and draws one.
pub(crate) fn derive(inputs: &Inputs<'_>) -> ToolsModel {
    let status = match (inputs.modules.is_empty(), inputs.modules_ready) {
        (true, false) => ToolsStatus::Loading,
        (true, true) => ToolsStatus::Empty,
        (false, _) => ToolsStatus::Ready,
    };
    let mut sections = Vec::new();
    let mut developer = Vec::new();
    // Mask mode replaces the module sections with the Masks panel and the adjustments that can
    // apply through a mask: a module with no maskable effect has nothing to offer a mask, so
    // offering its controls there would be offering an edit the mask cannot carry.
    // A pick taken on a mask keeps the mask workspace: the target stays bound to that mask.
    let masking = crate::state::canvas::mask_workspace(&inputs.session.workspace.mode)
        || inputs.target.is_some();
    for module in inputs.modules {
        if masking && !module.effects.iter().any(|effect| effect.maskable) {
            continue;
        }
        if !applies(module, inputs.document.state.as_ref()) {
            continue;
        }
        if !draws_section(module) {
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
        target.push(section(module, inputs));
    }
    ToolsModel {
        sections,
        developer,
        status,
        menu: inputs.view_state.menu.clone(),
    }
}

impl ToolsModel {
    /// Every section in registry order, developer sections last.
    pub(crate) fn all(&self) -> impl Iterator<Item = &SectionModel> {
        self.sections.iter().chain(self.developer.iter())
    }
}

/// Geometry diagnostics are host data: the desktop only formats the reported stage covers.
fn geometry_summary(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> Option<String> {
    if !module
        .effects
        .iter()
        .any(|effect| effect.stage == EffectStage::Geometry)
    {
        return None;
    }
    let geometry = inputs.document.recipe.as_ref()?.geometry.as_ref()?;
    let cover = &geometry["cover"];
    let combined = cover["combined"].as_f64()?;
    if combined <= 1.0 {
        return None;
    }
    Some(format!(
        "Lens ×{:.3} · Perspective ×{:.3} · cover ×{combined:.3}",
        cover["lens"].as_f64()?,
        cover["perspective"].as_f64()?
    ))
}

/// One module's section.
fn section(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> SectionModel {
    let expanded = expanded(module, inputs);
    let unavailable = match &module.availability {
        luxforge_core::Availability::Available => None,
        luxforge_core::Availability::Unavailable { reason } => Some(reason.clone()),
    };
    let disabled_reason = disabled_reason(unavailable.as_deref(), inputs);
    let enabled = disabled_reason.is_none();
    let active = active(module, inputs);
    let layout = section_layout(module, inputs);
    let scope = scope(module, inputs);
    let mut controls = Vec::new();
    // A declared crop frame is a host interaction, not a control: the host renders its draft panel
    // here and the module's own controls, Reset crop included, still come below.
    if let Some(frame) = crop_frame(inputs.modules).filter(|frame| frame.module.id == module.id) {
        controls.push(ControlModel::CropFrame(Box::new(crop_section(
            &frame, inputs, enabled,
        ))));
    }
    // A stacked module whose controls are one group draws that group's controls directly: a
    // header naming the only group repeats the band above it. The children keep their declared
    // paths under the group, so a nested group's key and reset still name its real position.
    let headerless = headerless_group(module);
    let headerless_reset = headerless
        .and(module.controls.first())
        .and_then(|group| group_reset(&module.id, group, inputs));
    match headerless {
        Some(children) => {
            for (index, control) in children.iter().enumerate() {
                controls.push(control_model(
                    ControlOwner::Module(module),
                    control,
                    inputs,
                    enabled,
                    &[0, index],
                ));
            }
            // A curve that is the headerless group's only control is named by the band above
            // it, so it draws no label line of its own.
            if let [ControlModel::Curve(curve)] = controls.as_mut_slice() {
                curve.label_shown = false;
            }
        }
        None => {
            for (index, control) in module.controls.iter().enumerate() {
                controls.push(control_model(
                    ControlOwner::Module(module),
                    control,
                    inputs,
                    enabled,
                    &[index],
                ));
            }
        }
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
        headerless_reset,
        capability: capabilities::section(module, inputs),
        controls,
        layout,
        status: (inputs.draft.is_some() && owns_mode(module, inputs)).then(|| "Draft".to_owned()),
        geometry_summary: geometry_summary(module, inputs),
        scope: scope.map(str::to_owned),
        enabled,
        disabled_reason,
    }
}

/// Whether a module has anything of its own to draw: controls, a capability block, or the host's
/// crop-frame editor its canvas declares. A module with none — the RAW development, whose controls
/// are Basic's variants — has no section.
pub(crate) fn draws_section(module: &ModuleDescriptor) -> bool {
    !module.controls.is_empty()
        || capabilities::declares(module)
        || matches!(module.canvas, Some(CanvasInteraction::CropFrame { .. }))
}

/// The controls of the one group a stacked module's controls consist of, when that is their whole
/// shape. The panel draws them flush under the module's band with no sub-group header, and so
/// with no disclosure, no collapse state and no Original or Custom caption: the band already
/// names the module, carries its reset and its active dot. The descriptor and the API are
/// unchanged; this is how the desktop lays such a module out. A tabbed module is not affected,
/// since its groups are its tabs.
pub(crate) fn headerless_group(module: &ModuleDescriptor) -> Option<&[Control]> {
    if module.layout == luxforge_core::ModuleLayout::Tabs {
        return None;
    }
    match module.controls.as_slice() {
        [Control::Group(luxforge_core::GroupControl { controls, .. })] => Some(controls),
        _ => None,
    }
}

/// The reset `group`, declared by `owner`, runs on the open photo through the bound target. A
/// group's reset resolves through the same rule as its controls: on a RAW photo's global target,
/// White balance's reset is the RAW development's As shot.
fn group_reset(owner: &str, group: &Control, inputs: &Inputs<'_>) -> Option<ResetRef> {
    ResetRef::of(
        luxforge_core::resolve_group_reset(
            owner,
            group,
            source_kind(inputs.document.state.as_ref()),
            inputs.target,
        )
        .map(|resolved| resolved.reset),
    )
}

/// The declared group at `path` is drawn without a header, so it has nothing to collapse.
pub(crate) fn is_headerless_group(module: &ModuleDescriptor, path: &[usize]) -> bool {
    path == [0] && headerless_group(module).is_some()
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
    if module.layout != luxforge_core::ModuleLayout::Tabs {
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

/// Why editing this module is disabled, in the words the status bar would use: its provider's
/// unavailability first, then the one editability rule ([`Inputs::edit_refusal`]).
fn disabled_reason(unavailable: Option<&str>, inputs: &Inputs<'_>) -> Option<String> {
    unavailable
        .map(str::to_owned)
        .or_else(|| inputs.edit_refusal.clone())
}

/// The current recipe holds a layer of one of this module's effects **for the bound target**, and
/// that layer does something.
///
/// The target is what makes the section's dot honest while a mask is open: a global Basic layer says
/// nothing about whether this mask's Basic layer is doing anything, and vice versa. Whether a layer
/// does something is the core's answer, each row's `neutral` from `recipe.describe`, so a field
/// patch returned to its neutral values, a whole-image crop, the identity orientation and a RAW
/// development at As shot and 0 EV are stored but carry no dot. The rows are the current entry's,
/// never a historical preview's.
///
/// A section reads every module its resolved controls edit ([`providers`]): on a RAW photo's global
/// target that is Basic and the RAW development, so a custom white balance lights Basic's dot.
fn active(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> bool {
    providers(module, inputs)
        .into_iter()
        .any(|provider| edits(provider, inputs))
}

/// The current recipe holds a non-neutral layer of one of `module`'s effects for the bound target.
fn edits(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> bool {
    let (Some(_), Some(recipe)) = (
        inputs.document.state.as_ref(),
        inputs.document.current_recipe.as_ref(),
    ) else {
        return false;
    };
    recipe
        .layers
        .iter()
        .filter(|row| row.mask.as_ref() == inputs.target)
        .any(|row| module.effects.iter().any(|effect| effect.id == row.effect) && !row.neutral)
}

/// The name of the mask the generated sections are bound to, as the displayed entry's listing
/// names it, or `None` when they are bound to the global layer. A target the listing does not hold
/// (a selection a moment before the listing catches up) names nothing rather than an id.
pub(crate) fn bound_mask_name<'a>(inputs: &Inputs<'a>) -> Option<&'a str> {
    let target = inputs.target?;
    inputs
        .document
        .masks
        .as_ref()
        .filter(|listing| Some(&listing.entry_id) == inputs.document.display_entry.as_ref())?
        .masks
        .iter()
        .find(|report| &report.id == target)
        .map(|report| report.name.as_str())
}

/// The scope chip one section's band carries: the bound mask's name on a module with a maskable
/// effect, and nothing on any other, because only a maskable module's controls edit through it.
fn scope<'a>(module: &ModuleDescriptor, inputs: &Inputs<'a>) -> Option<&'a str> {
    if !module.effects.iter().any(|effect| effect.maskable) {
        return None;
    }
    bound_mask_name(inputs)
}

/// One declared control as the panel models it. `owner` says whose declarations resolve its
/// parameter: the declaring module's, or the host's own `mask.*` family for a host control.
///
/// A module's control is first resolved for the open photo and the bound target through the core's
/// one rule ([`resolved`]): where a variant applies, the control drawn is the variant's, over its
/// module's own actions, values and canvas, in the declared control's place and under its label.
pub(crate) fn control_model(
    owner: ControlOwner<'_>,
    control: &Control,
    inputs: &Inputs<'_>,
    enabled: bool,
    path: &[usize],
) -> ControlModel {
    let ControlOwner::Module(module) = owner else {
        return resolved_model(owner, None, control, inputs, enabled, path);
    };
    let kind = source_kind(inputs.document.state.as_ref());
    match resolved(inputs.modules, module, control, kind, inputs.target) {
        Some((provider, variant)) if provider.id != module.id => resolved_model(
            ControlOwner::Module(provider),
            Some(module),
            variant,
            inputs,
            enabled && provider.is_available(),
            path,
        ),
        Some(_) => resolved_model(owner, None, control, inputs, enabled, path),
        None => ControlModel::Unsupported(format!(
            "a {} control of {} whose providing module is not registered",
            control.kind_name(),
            module.title
        )),
    }
}

/// One control as it applies, modelled against `owner`'s declarations. `declarer` is the module
/// that declared the control when `owner` provides it as a variant: a picker variant's letter is
/// the declaring module's when the providing canvas has none of its own.
fn resolved_model(
    owner: ControlOwner<'_>,
    declarer: Option<&ModuleDescriptor>,
    control: &Control,
    inputs: &Inputs<'_>,
    enabled: bool,
    path: &[usize],
) -> ControlModel {
    match control {
        Control::Group(group) => {
            let reset = group_reset(owner.id(), control, inputs);
            let controls: Vec<ControlModel> = group
                .controls
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let mut child_path = path.to_vec();
                    child_path.push(index);
                    control_model(owner, child, inputs, enabled, &child_path)
                })
                .collect();
            ControlModel::Group(GroupControl {
                label: group.label.clone(),
                reset,
                path: path.to_vec(),
                expanded: inputs
                    .control_ui
                    .group_expanded
                    .get(&group_key(owner.id(), path))
                    .copied()
                    .unwrap_or(!group.collapsed),
                state: group_state(&controls, inputs),
                controls,
            })
        }
        // A declared field reset is resolved from the descriptors when the reset is asked for, by
        // `fields::field_reset`; the slider itself draws nothing for it.
        Control::Number(number) => {
            let mut model = value_model(
                owner,
                inputs,
                &number.action,
                &number.parameter,
                &number.label,
            );
            if let ControlModel::Slider(slider) = &mut model {
                slider.reset = ResetRef::of(number.reset.as_ref());
                slider.style = match number.style {
                    NumberStyle::Slider => NumberControlStyle::Slider,
                    NumberStyle::Field => NumberControlStyle::Field,
                    NumberStyle::Stepper => NumberControlStyle::Stepper { rail: false },
                };
                slider.rail = rail_style(number.rail.as_ref());
            }
            model
        }
        Control::Range(range) => {
            let action = range.action.as_str();
            // Each edge and shoulder is modelled exactly as its own number field is, as a field,
            // under the label its own number control declares. A parameter that is not a number
            // field names the band's problem in the band's place, as a lone field would.
            let mut fields: [Option<SliderControl>; 4] = Default::default();
            for (slot, parameter) in fields.iter_mut().zip([
                Some(range.low.as_str()),
                Some(range.high.as_str()),
                range.low_feather.as_deref(),
                range.high_feather.as_deref(),
            ]) {
                let Some(parameter) = parameter else { continue };
                let label = owner.field_label(action, parameter);
                match value_model(owner, inputs, action, parameter, &label) {
                    ControlModel::Slider(mut field) => {
                        field.style = NumberControlStyle::Field;
                        field.reset =
                            ResetRef::of(declared_field_reset(inputs.modules, action, parameter));
                        *slot = Some(field);
                    }
                    unsupported => return unsupported,
                }
            }
            let [Some(low), Some(high), low_feather, high_feather] = fields else {
                unreachable!("both edges are always modelled or returned early");
            };
            ControlModel::Range(Box::new(RangeControl {
                action: action.to_owned(),
                label: range.label.clone(),
                rail: rail_style(range.rail.as_ref()),
                low,
                high,
                low_feather,
                high_feather,
            }))
        }
        Control::QueryChoice(control) => {
            let ui = inputs
                .control_ui
                .query_choices
                .get(&control.action)
                .cloned()
                .unwrap_or_default();
            let shared = control
                .shared
                .iter()
                .filter(|name| ui.shows_shared(name))
                .filter_map(|name| {
                    let parameter = owner.parameter(&control.action, name)?.clone();
                    let text = inputs
                        .fields
                        .get(&control.action, name)
                        .map(str::to_owned)
                        .unwrap_or_default();
                    Some(super::query_choice::SharedInput { parameter, text })
                })
                .collect();
            ControlModel::QueryChoice(Box::new(super::query_choice::QueryChoiceModel {
                control: control.clone(),
                ui,
                shared,
                enabled,
            }))
        }
        Control::Toggle(toggle) => value_model(
            owner,
            inputs,
            &toggle.action,
            &toggle.parameter,
            &toggle.label,
        ),
        Control::Choice(choice) => {
            let mut model = value_model(
                owner,
                inputs,
                &choice.action,
                &choice.parameter,
                &choice.label,
            );
            if let ControlModel::Enum(field) = &mut model {
                field.style = choice_style(choice.style, field.options.len());
            }
            model
        }
        Control::Color(color) => {
            let mut model =
                value_model(owner, inputs, &color.action, &color.parameter, &color.label);
            if let ControlModel::Color(field) = &mut model {
                field.style = match color.style {
                    ColorStyle::Fields => ColorControlStyle::Fields,
                    ColorStyle::Picker => ColorControlStyle::Picker,
                };
            }
            model
        }
        Control::Curve(curve) => curve_model(inputs, curve),
        Control::Action(button) => {
            let action = button.action.as_str();
            let params = declared_action(inputs.modules, action)
                .map(|declared| action_params(declared, &button.preset, inputs.fields));
            ControlModel::Action(ActionControl {
                action: action.to_owned(),
                label: button.label.clone(),
                preset: button.preset.clone(),
                runnable: enabled && matches!(params, Some(Ok(_))),
                reason: match params {
                    Some(Err(message)) => Some(message),
                    Some(Ok(_)) => None,
                    None => Some(format!("No module declares the action {action}")),
                },
                style: match button.style {
                    ActionStyle::Default => ActionControlStyle::Default,
                    ActionStyle::Primary => ActionControlStyle::Primary,
                    ActionStyle::Icon => ActionControlStyle::Icon,
                },
                icon: button.icon.clone(),
            })
        }
        // The picker reads its mode's name and letter from the same canvas declaration the keymap
        // binds, so the panel and the keyboard always agree about what the mode is called.
        // A picker provided as a variant enters its providing module's canvas and answers to the
        // declaring module's letter, which the keyboard binds to the same mode ([`mode_shortcuts`]).
        Control::Picker(picker) => {
            let Some(module) = owner.descriptor() else {
                return ControlModel::Unsupported("a picker needs a declaring module".into());
            };
            let letter = |module: &ModuleDescriptor| {
                module
                    .canvas
                    .as_ref()
                    .and_then(CanvasInteraction::shortcut)
                    .map(str::to_owned)
            };
            ControlModel::Picker(PickerControl {
                module_id: module.id.clone(),
                label: picker.label.clone(),
                title: module
                    .canvas
                    .as_ref()
                    .map(CanvasInteraction::title)
                    .unwrap_or(&picker.label)
                    .to_owned(),
                shortcut: letter(module).or_else(|| declarer.and_then(letter)),
                selected: owns_mode(module, inputs),
                // Leaving the pick returns to where it was entered from: a pick on a mask is
                // taken from the Masks panel and goes back to it.
                target: match (owns_mode(module, inputs), inputs.target) {
                    (true, Some(_)) => luxforge_core::MASK_MODE.to_owned(),
                    (true, None) => luxforge_core::POINTER_MODE.to_owned(),
                    (false, _) => module.id.clone(),
                },
                enabled,
            })
        }
        Control::Task(task) => {
            let Some(module) = owner.descriptor() else {
                return ControlModel::Unsupported("a task needs a declaring module".into());
            };
            ControlModel::Task(capabilities::task_control(
                module,
                &task.task,
                &task.label,
                inputs,
                enabled,
            ))
        }
        // The library is host data beside the recipe; the module declares only where it goes and
        // which of its actions a row submits.
        Control::Presets(presets) => {
            let Some(module) = owner.descriptor() else {
                return ControlModel::Unsupported(
                    "a preset library needs a declaring module".into(),
                );
            };
            let unavailable = match &module.availability {
                luxforge_core::Availability::Available => None,
                luxforge_core::Availability::Unavailable { reason } => Some(reason.as_str()),
            };
            let reason = disabled_reason(unavailable, inputs);
            ControlModel::Presets(Box::new(presets_model(
                &presets.action,
                inputs,
                enabled,
                reason.as_deref(),
            )))
        }
    }
}

/// The rail a number or range control declares, as the view draws it.
fn rail_style(rail: Option<&RailDecoration>) -> RailStyle {
    match rail.unwrap_or(&RailDecoration::Plain) {
        RailDecoration::Plain => RailStyle::Plain,
        RailDecoration::Hue => RailStyle::Hue,
        RailDecoration::Temperature => RailStyle::Temperature,
        RailDecoration::Tint => RailStyle::Tint,
        RailDecoration::Gradient { stops } => RailStyle::Gradient(stops.clone()),
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
            != Some(crate::state::fields::seed_text(parameter_desc).as_str());
        true
    }
    for control in walk(controls) {
        let mut patch_field = |action: &str, parameter: &str| {
            field(action, parameter, inputs, &mut values, &mut custom)
        };
        let all_patch_fields = match control {
            // A field whose reset is an action of its own is at its neutral exactly when the
            // layer it mirrors is: its text cannot say so. While it is dragged, the draft has
            // already left that layer's committed state, as a dragged field-patch slider's text
            // has left its default.
            ControlModel::Slider(slider) if slider.reset.is_some() => {
                let owner = inputs
                    .modules
                    .iter()
                    .find(|module| module.action(&slider.action).is_some_and(|a| a.patch));
                match owner {
                    Some(owner) => {
                        values += 1;
                        custom |= edits(owner, inputs)
                            || inputs.dragging.is_some_and(|(action, parameter)| {
                                *action == slider.action && *parameter == slider.parameter
                            });
                        true
                    }
                    None => false,
                }
            }
            ControlModel::Slider(slider) => patch_field(&slider.action, &slider.parameter),
            ControlModel::Toggle(toggle) => patch_field(&toggle.action, &toggle.parameter),
            ControlModel::Enum(choice) => patch_field(&choice.action, &choice.parameter),
            ControlModel::Color(color) => patch_field(&color.action, &color.parameter),
            ControlModel::Curve(curve) => curve
                .channels
                .iter()
                .all(|channel| patch_field(&curve.action, &channel.parameter)),
            _ => true,
        };
        if !all_patch_fields {
            return None;
        }
    }
    if values == 0 {
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
    owner: ControlOwner<'_>,
    inputs: &Inputs<'_>,
    action: &str,
    parameter: &str,
    label: &str,
) -> ControlModel {
    let Some(declared) = owner.parameter(action, parameter) else {
        return ControlModel::Unsupported(undeclared_label(action, parameter));
    };
    let text = inputs.fields.get(action, parameter).unwrap_or_default();
    let invalid = parse_field(declared, text).err();
    let typing = inputs
        .editing
        .is_some_and(|(a, p)| a == action && p == parameter);
    match &declared.kind {
        ParameterKind::Integer { .. } | ParameterKind::Number { .. } => {
            let spec = NumberSpec::of(declared).expect("a number parameter has a number spec");
            ControlModel::Slider(slider(
                action, parameter, label, declared, spec, text, invalid, typing, inputs,
            ))
        }
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
            let local = inputs
                .control_ui
                .color(&(action.to_owned(), parameter.to_owned()));
            let picker_hsv = local
                .and_then(|local| local.hsv)
                .filter(|picker| picker.rgb == rgb)
                .map(|picker| picker.hsv);
            let dragging = inputs
                .dragging
                .is_some_and(|(a, p)| a == action && p == parameter);
            ControlModel::Color(ColorControl {
                action: action.to_owned(),
                parameter: parameter.to_owned(),
                ids: [0, 1, 2].map(|index| {
                    field_id(
                        action,
                        parameter,
                        Some(crate::state::fields::CHANNELS[index]),
                    )
                }),
                label: labelled(label, declared),
                channels: [0, 1, 2].map(|index| channel_text(text, index).to_owned()),
                text: text.to_owned(),
                invalid,
                style: ColorControlStyle::Fields,
                rgb,
                picker_hsv,
                picker_open: local.is_some_and(|local| local.open),
                dragging,
                hex_edit: ValueEdit::typed(local.and_then(|local| local.hex.as_ref())),
                channel_edits: [0, 1, 2].map(|index| {
                    ValueEdit::typed(local.and_then(|local| local.channels[index].as_ref()))
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
        // An artifact is published by a task and committed with its result, never typed.
        ParameterKind::Artifact => ControlModel::Unsupported(format!(
            "artifact parameter {parameter} of action {action} is filled by a task, not a control"
        )),
        ParameterKind::String { .. } => ControlModel::Unsupported(format!(
            "string parameter {parameter} of action {action} needs a text control"
        )),
        ParameterKind::Settings => ControlModel::Unsupported(format!(
            "settings parameter {parameter} of action {action} needs a presets control"
        )),
        // A path has no control and is never meant to get one: it is drawn on the canvas, so a
        // panel that finds one declared says so rather than inventing a widget for it.
        ParameterKind::Points { .. } => ControlModel::Unsupported(format!(
            "points parameter {parameter} of action {action} is drawn on the canvas and has no \
             panel control"
        )),
        // Registration refuses both on an action: only a module setting declares one.
        ParameterKind::Endpoint { .. } | ParameterKind::Secret { .. } => {
            ControlModel::Unsupported(format!(
                "{} parameter {parameter} of action {action} is a module setting, not a control",
                declared.kind.name()
            ))
        }
        // An identity names the object a request addresses; the panel's selection supplies it.
        ParameterKind::Identity { .. } => ControlModel::Unsupported(format!(
            "identity parameter {parameter} of action {action} is supplied by the selection, not \
             a control"
        )),
        // Registration refuses both on an action: only a host method declares one.
        ParameterKind::Text { .. } | ParameterKind::Json => ControlModel::Unsupported(format!(
            "{} parameter {parameter} of action {action} belongs to a host method, not a control",
            declared.kind.name()
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn slider(
    action: &str,
    parameter: &str,
    label: &str,
    declared: &ParameterDescriptor,
    spec: NumberSpec,
    text: &str,
    invalid: Option<String>,
    typing: bool,
    inputs: &Inputs<'_>,
) -> SliderControl {
    let value = parse_field(declared, text)
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(spec.min);
    SliderControl {
        action: action.to_owned(),
        parameter: parameter.to_owned(),
        id: field_id(action, parameter, None),
        // The value carries the unit, so the label does not repeat it.
        label: label.to_owned(),
        unit: declared.unit.as_deref().map(unit_symbol),
        spec,
        style: NumberControlStyle::Slider,
        rail: RailStyle::Plain,
        value,
        display: if invalid.is_some() {
            text.to_owned()
        } else {
            spec.format(value)
        },
        edit: if typing {
            ValueEdit::Typing(text.to_owned())
        } else {
            ValueEdit::None
        },
        dragging: inputs
            .dragging
            .is_some_and(|(a, p)| a == action && p == parameter),
        invalid,
        default: crate::state::fields::seed_text(declared),
        reset: None,
    }
}

/// A declared unit as a value shows it: `deg` is the degree sign, which sits against the number
/// (`2.4°`); every other unit is shown as declared.
fn unit_symbol(unit: &str) -> String {
    match unit {
        "deg" => "\u{b0}".to_owned(),
        unit => unit.to_owned(),
    }
}

fn curve_model(inputs: &Inputs<'_>, curve: &luxforge_core::CurveControl) -> ControlModel {
    let action = curve.action.as_str();
    let channels = &curve.channels;
    let id = (
        action.to_owned(),
        channels
            .first()
            .map(|channel| channel.parameter.clone())
            .unwrap_or_default(),
    );
    let local = inputs.control_ui.curve(&id);
    let selected_channel = local
        .map_or(0, |local| local.channel)
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
    let (points_max, hint) = match declared.map(|declared| &declared.kind) {
        Some(ParameterKind::Curve {
            points_min,
            points_max,
            fixed_x,
            ..
        }) => (
            *points_max,
            (fixed_x.is_none() && points_min != points_max).then(|| CURVE_HINT.to_owned()),
        ),
        _ => (0, None),
    };
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
    let selected_point = local
        .and_then(|local| local.point)
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
                ValueEdit::typed(
                    local.and_then(|local| local.edits.get(&(parameter.to_owned(), index, axis))),
                )
            }),
        })
        .collect();
    let samples = local
        .and_then(|local| local.samples.get(parameter))
        .filter(|samples| {
            parsed.as_ref() == Some(&samples.source)
                && inputs.document.display_entry.as_ref() == Some(&samples.entry)
                && inputs
                    .document
                    .state
                    .as_ref()
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
        label: curve.label.clone(),
        label_shown: true,
        channels: channels
            .iter()
            .map(|channel| CurveChannelModel {
                parameter: channel.parameter.clone(),
                label: channel.label.clone(),
            })
            .collect(),
        sample_query: curve.sample_query.clone(),
        background: matches!(curve.background, CurveBackground::Histogram),
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
        points_open: local.is_some_and(|local| local.points_open),
        points_max,
        hint,
    })
}

/// The crop section, generated from the declared crop-frame interaction: the draft's controls while
/// a draft is open, and the same controls reading the displayed entry's committed crop while idle.
fn crop_section(frame: &CropFrame<'_>, inputs: &Inputs<'_>, enabled: bool) -> CropSectionModel {
    let presets = frame.presets();
    let base = CropSectionModel {
        title: frame.title.to_owned(),
        paused: !super::at_current(inputs.document.state.as_ref(), inputs.session),
        custom: (
            inputs.crop_section.custom.0.clone(),
            inputs.crop_section.custom.1.clone(),
        ),
        custom_ids: (
            field_id(frame.fit_action, "custom-width", None),
            field_id(frame.fit_action, "custom-height", None),
        ),
        guide: inputs.crop_section.guide,
        enabled,
        ..CropSectionModel::default()
    };
    let Some(draft) = inputs.draft else {
        // Idle, the box and the rail show the committed angle, and the box shows what is being
        // typed while it is open.
        let committed = committed_crop(frame, inputs);
        let angle = angle_control(frame, inputs, committed.angle, false);
        let locked = committed.aspect.is_some();
        let chosen = committed
            .aspect
            .as_ref()
            .map_or(luxforge_core::CropAspect::FREE, |(option, _)| {
                option.as_str()
            });
        return CropSectionModel {
            presets: preset_chips(&presets, chosen),
            angle,
            lock_label: lock_label(locked),
            locked,
            can_swap: enabled && locked,
            ..base
        };
    };
    CropSectionModel {
        drafting: true,
        conflicted: inputs.gesture_conflicted,
        presets: preset_chips(&presets, &draft.preset),
        lock_label: lock_label(draft.aspect.ratio().is_some()),
        locked: draft.aspect.ratio().is_some(),
        can_swap: enabled && draft.aspect.ratio().is_some(),
        can_apply: inputs.apply_refusal.is_none(),
        can_reapply: !inputs.busy,
        readout: readout(draft, frame.action),
        angle: angle_control(frame, inputs, draft.stage.angle, true),
        ..base
    }
}

/// The crop angle as the generic stepper, with the rail between its buttons: `angle` on the rail
/// and in the box, formatted as its parameter declares, and the text as typed while the box is
/// open. The value never follows the typed text, so the rail stays on the frame's angle until a
/// number is submitted. `live` marks the rail's gesture live: the draft is open.
fn angle_control(
    frame: &CropFrame<'_>,
    inputs: &Inputs<'_>,
    angle: f64,
    live: bool,
) -> Option<SliderControl> {
    let declared = frame.module.action(frame.action)?.parameter(frame.angle)?;
    let spec = NumberSpec::of(declared)?;
    let typing = inputs
        .editing
        .is_some_and(|(action, parameter)| action == frame.action && parameter == frame.angle);
    let text = if typing {
        inputs
            .fields
            .get(frame.action, frame.angle)
            .unwrap_or_default()
            .to_owned()
    } else {
        spec.format(angle)
    };
    let invalid = if typing {
        parse_field(declared, &text).err()
    } else {
        None
    };
    let mut control = slider(
        frame.action,
        frame.angle,
        "",
        declared,
        spec,
        &text,
        invalid,
        typing,
        inputs,
    );
    control.value = angle;
    control.style = NumberControlStyle::Stepper { rail: true };
    control.dragging = live;
    Some(control)
}

/// One chip per declared ratio preset, with `chosen` the option that reads selected.
pub(crate) fn preset_chips(presets: &[AspectPreset], chosen: &str) -> Vec<PresetChip> {
    presets
        .iter()
        .enumerate()
        .map(|(index, preset)| PresetChip {
            index,
            label: preset.label(),
            chosen: preset.option == chosen,
        })
        .collect()
}

fn lock_label(locked: bool) -> String {
    if locked {
        "Unlock ratio".into()
    } else {
        "Lock ratio".into()
    }
}

/// What the idle crop section reads from the displayed entry's committed crop: its straightening
/// angle and the declared ratio its rectangle reads as, which is exactly what a draft opened on it
/// seeds ([`CropDraft::from_layer`]). No crop layer, a neutral one or one whose values cannot be
/// read is no crop: Free at 0°, as a draft on a stack without one starts.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CommittedCrop {
    pub(crate) angle: f64,
    /// The declared `aspect` option the committed rectangle reads as, with the ratio it locks.
    pub(crate) aspect: Option<(String, f64)>,
}

/// The displayed entry's committed crop, as the idle section shows it, read from that entry's own
/// `recipe.describe` row. The draft that Start opens edits the stack's first crop layer, so this
/// reads that same one: its `neutral`, its `values` (the stored rectangle and angle, read under the
/// declared parameter names) and its `input_stage`, the stage the core's own stage fold says the
/// crop receives, whatever geometry precedes it. The desktop folds no geometry and parses no
/// payload itself. Rows that describe another entry, which is what the desktop holds until the
/// displayed entry's rows arrive, read as no crop, and a row without a stage (a provider before it
/// the core cannot compile) reads as Free at its own angle. Reading it is `O(layers)` over rows
/// already in hand: no render, no sample, no request.
pub(crate) fn committed_crop(frame: &CropFrame<'_>, inputs: &Inputs<'_>) -> CommittedCrop {
    let displayed = inputs.document.display_entry.as_ref().or_else(|| {
        inputs
            .document
            .state
            .as_ref()
            .map(|state| &state.current_entry.id)
    });
    let Some(recipe) = inputs
        .document
        .recipe
        .as_ref()
        .filter(|recipe| Some(&recipe.entry_id) == displayed)
    else {
        return CommittedCrop::default();
    };
    let Some((_, row)) = crop_row(frame, recipe) else {
        return CommittedCrop::default();
    };
    let Some(payload) = frame.payload(&row.values) else {
        return CommittedCrop::default();
    };
    if row.neutral {
        return CommittedCrop::default();
    }
    let aspect = row.input_stage.and_then(|stage| {
        let output = payload
            .output_rect(&CropStage {
                width: stage.width,
                height: stage.height,
                angle: payload.angle,
            })
            .ok()?;
        committed_aspect(
            &frame.presets(),
            (stage.width, stage.height),
            (output.width, output.height),
        )
        .map(|(preset, ratio)| (preset.option.clone(), ratio))
    });
    CommittedCrop {
        angle: payload.angle,
        aspect,
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

/// The declaration one action identity carries, whether a module declares it or the host does.
///
/// The `mask.*` family is declared with the same [`ActionDescriptor`] type a module uses, so every
/// generic path below it — the request builder, the patch rule, the draft rule, the field store and
/// the generated controls — works on a mask command without a second copy of itself. A method name
/// carries a dot, which an action identity may not, so the two namespaces cannot collide.
pub(crate) fn declared_action<'a>(
    modules: &'a [ModuleDescriptor],
    action: &str,
) -> Option<&'a ActionDescriptor> {
    if let Some(command) = luxforge_core::mask::commands::find(action) {
        return Some(&command.action);
    }
    modules.iter().find_map(|module| module.action(action))
}

/// Who declares the control being modelled: one module, or the host's own mask command family.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ControlOwner<'a> {
    Module(&'a ModuleDescriptor),
    /// A host control, whose action is looked up in the mask command table.
    Host,
}

impl<'a> ControlOwner<'a> {
    /// The declared parameter this control edits. A module's control is resolved against its own
    /// declarations only, so a control naming another module's action is reported rather than drawn.
    pub(crate) fn parameter(
        self,
        action: &str,
        parameter: &str,
    ) -> Option<&'a ParameterDescriptor> {
        match self {
            Self::Module(module) => module.action(action)?.parameter(parameter),
            Self::Host => luxforge_core::mask::commands::find(action)?
                .action
                .parameter(parameter),
        }
    }

    /// The label this owner's own number control declares for one field, else the parameter's
    /// name: what a band's field is called under the band.
    fn field_label(self, action: &str, parameter: &str) -> String {
        let controls = match self {
            Self::Module(module) => &module.controls[..],
            Self::Host => luxforge_core::mask::commands::controls(),
        };
        labelled_control(controls, action, parameter)
            .unwrap_or(parameter)
            .to_owned()
    }

    /// The module's own id, for the per-module keys a group's expansion and a tab selection use.
    fn id(self) -> &'a str {
        match self {
            Self::Module(module) => &module.id,
            Self::Host => "mask",
        }
    }

    fn descriptor(self) -> Option<&'a ModuleDescriptor> {
        match self {
            Self::Module(module) => Some(module),
            Self::Host => None,
        }
    }
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

/// The reset a number control declares for its own field, if the first number control of this
/// action and parameter declares one: the action and preset that resetting the field runs instead
/// of its parameter's declared default.
pub(crate) fn declared_field_reset<'a>(
    modules: &'a [ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<&'a ResetAction> {
    // A variant's field reset counts: on a RAW photo Temperature's double-click is the RAW
    // development's As shot, which its variant declares.
    modules
        .iter()
        .find_map(|module| {
            with_variants(&module.controls).find_map(|control| match control {
                Control::Number(number)
                    if number.action == action && number.parameter == parameter =>
                {
                    Some(number.reset.as_ref())
                }
                _ => None,
            })
        })
        .flatten()
}

/// The label a generated control carries for one field, as the panel and the status line name it.
///
/// The host's own `mask.*` controls are searched first, for the same reason `declared_action` looks
/// there first: a mask command is declared with the same types and its controls carry the same
/// labels, so a status line naming a dragged field must find one wherever it was declared.
pub(crate) fn control_label(
    modules: &[ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<String> {
    labelled_control(luxforge_core::mask::commands::controls(), action, parameter)
        .or_else(|| {
            modules
                .iter()
                .find_map(|module| labelled_control(&module.controls, action, parameter))
        })
        .map(str::to_owned)
}

pub(crate) fn labelled_control<'a>(
    controls: &'a [Control],
    action: &str,
    parameter: &str,
) -> Option<&'a str> {
    let field = |declared: &str, named: &str, label: &'a str| {
        (declared == action && named == parameter).then_some(label)
    };
    with_variants(controls).find_map(|control| match control {
        Control::Number(number) => field(&number.action, &number.parameter, &number.label),
        Control::Color(color) => field(&color.action, &color.parameter, &color.label),
        Control::Toggle(toggle) => field(&toggle.action, &toggle.parameter, &toggle.label),
        Control::Choice(choice) => field(&choice.action, &choice.parameter, &choice.label),
        Control::QueryChoice(choice) => (choice.action == action
            && choice.shared.iter().any(|name| name == parameter))
        .then_some(choice.label.as_str()),
        Control::Curve(curve) => (curve.action == action
            && curve
                .channels
                .iter()
                .any(|channel| channel.parameter == parameter))
        .then_some(curve.label.as_str()),
        // A band's fields carry the labels of their own number controls, and no other kind labels
        // a field.
        Control::Group(_)
        | Control::Range(_)
        | Control::Action(_)
        | Control::Picker(_)
        | Control::Task(_)
        | Control::Presets(_) => None,
    })
}

/// The JSON method an action id is published as: `edit.<id>` for a module action, and the id itself
/// for a host `mask.*` command, whose method name *is* its action identity.
///
/// It lives here rather than in the view because "which method does this control send" is a fact about
/// the host's command table, and the view layer holds no core dependency: a control's menu target
/// carries it from the moment the menu opens ([`MenuTarget::control`]). Reading it from that table
/// is also what keeps the caption a person copies and the method the request carries from drifting
/// apart when a kind is registered.
pub(crate) fn published_method(action: &str) -> String {
    if luxforge_core::mask::commands::find(action).is_some() {
        action.to_owned()
    } else {
        format!("edit.{action}")
    }
}

impl MenuTarget {
    /// The menu target of one generated control, with the method its copied request carries.
    pub(crate) fn control(
        action: String,
        parameter: Option<String>,
        preset: Option<Map<String, Value>>,
    ) -> Self {
        let method = published_method(&action);
        Self::Control {
            action,
            parameter,
            preset,
            method,
        }
    }
}

/// Whether `module` applies to the photo `state` holds, by the core's one rule
/// ([`ModuleDescriptor::applies_to`]) for that photo's source kind: what the tools panel's
/// sections, the palette, the mode strip, the mode shortcuts and the canvas pick gate all read, so
/// none of them names a module. With no photo open, a module applies when it applies to every
/// kind, so nothing kind-specific is offered before a photo says which kind it is. `O(effects)`,
/// no allocation.
pub(crate) fn applies(module: &ModuleDescriptor, state: Option<&EditorState>) -> bool {
    match state {
        Some(state) => module.applies_to(state.asset.source.tag()),
        None => SourceTag::ALL
            .into_iter()
            .all(|kind| module.applies_to(kind)),
    }
}

/// The source kind of the photo `state` holds, which a control's variants resolve against; `None`
/// with no photo open, so every control is its declaring module's own.
pub(crate) fn source_kind(state: Option<&EditorState>) -> Option<SourceTag> {
    state.map(|state| state.asset.source.tag())
}

/// `control`, declared by `owner`, as it applies to the open photo and the bound target, through
/// the core's one rule ([`luxforge_core::resolve_control`]): the module that provides it, the
/// declaring one or a variant's, and the control itself. `None` when a variant names a module that
/// is not registered, which registration refuses, so it is reported rather than guessed around.
pub(crate) fn resolved<'a>(
    modules: &'a [ModuleDescriptor],
    owner: &'a ModuleDescriptor,
    control: &'a Control,
    kind: Option<SourceTag>,
    target: Option<&MaskId>,
) -> Option<(&'a ModuleDescriptor, &'a Control)> {
    let resolved = luxforge_core::resolve_control(&owner.id, control, kind, target);
    if !resolved.variant {
        return Some((owner, control));
    }
    Some((module_of(modules, resolved.module)?, resolved.control))
}

/// Every control a tree declares, each followed by the variants it carries: the whole vocabulary a
/// question such as "which label does this field carry" or "what resets it" is answered from,
/// whichever photo it is asked for. A variant is a leaf of its base's shape, never a group.
pub(crate) fn with_variants(controls: &[Control]) -> impl Iterator<Item = &Control> {
    walk(controls).flat_map(|control| {
        std::iter::once(control).chain(
            control
                .variants()
                .iter()
                .filter_map(|variant| variant.control.as_deref()),
        )
    })
}

/// The modules a section's controls edit on the open photo and the bound target: the section's own
/// module first, then each module a variant of its controls or group resets resolves to. The
/// section's edited dot, its digest and its captions read every one of them, so a RAW photo's
/// custom white balance lights Basic's dot. `O(controls)`, no allocation beyond the short list.
pub(crate) fn providers<'a>(
    module: &'a ModuleDescriptor,
    inputs: &Inputs<'a>,
) -> Vec<&'a ModuleDescriptor> {
    let kind = source_kind(inputs.document.state.as_ref());
    let mut providers = vec![module];
    for control in walk(&module.controls) {
        let variant = luxforge_core::resolve_control(&module.id, control, kind, inputs.target);
        let reset = luxforge_core::resolve_group_reset(&module.id, control, kind, inputs.target);
        for id in [
            variant.variant.then_some(variant.module),
            reset
                .filter(|reset| reset.variant)
                .map(|reset| reset.module),
        ]
        .into_iter()
        .flatten()
        {
            if !providers.iter().any(|provider| provider.id == id)
                && let Some(provider) = module_of(inputs.modules, id)
            {
                providers.push(provider);
            }
        }
    }
    providers
}

/// The canvas mode `module`'s picker control enters on the open photo and target: the module its
/// picker resolves to, which is `module` itself unless a variant provides the picker. `None` when
/// the module declares no picker.
fn picker_mode<'a>(
    modules: &'a [ModuleDescriptor],
    module: &'a ModuleDescriptor,
    kind: Option<SourceTag>,
    target: Option<&MaskId>,
) -> Option<&'a str> {
    let picker = walk(&module.controls).find(|control| matches!(control, Control::Picker(_)))?;
    resolved(modules, module, picker, kind, target).map(|(provider, _)| provider.id.as_str())
}

/// The pick modes the resolved picker controls of the modules that apply to the open photo name, on
/// the bound target: exactly the pick modes a person can reach from the panel and the keyboard. On
/// a RAW photo's global target Basic's picker names the RAW development's sensor pick, so Basic's
/// own sample-apply is not among them; on a mask, and on a JPEG, it is.
pub(crate) fn pick_modes<'a>(
    modules: &'a [ModuleDescriptor],
    state: Option<&EditorState>,
    target: Option<&MaskId>,
) -> Vec<&'a str> {
    let kind = source_kind(state);
    modules
        .iter()
        .filter(|module| module.is_available() && applies(module, state))
        .filter_map(|module| picker_mode(modules, module, kind, target))
        .collect()
}

/// Whether a click on the photograph may pick in `mode`: a host mode belongs to no module and
/// answers every photo, and a module's pick mode answers only while a resolved picker names it
/// ([`pick_modes`]), so a pick mode entered through the API that the panel would not offer on this
/// photo and target takes no click. The pick gate reads this; it names no module.
pub(crate) fn pick_reachable(
    modules: &[ModuleDescriptor],
    state: Option<&EditorState>,
    target: Option<&MaskId>,
    mode: &str,
) -> bool {
    match module_of(modules, mode) {
        None => true,
        Some(module) => {
            applies(module, state) && pick_modes(modules, state, target).contains(&mode)
        }
    }
}

/// The mode shortcut letters the keyboard answers: one per available module that declares a canvas
/// shortcut and applies to the photo `state` holds, with the mode it enters. A module whose picker
/// resolves to another module's pick on this photo and target enters that one, so Basic's `W` is
/// the RAW development's sensor pick on a RAW photo's global target and Basic's own elsewhere.
pub(crate) fn mode_shortcuts(
    modules: &[ModuleDescriptor],
    state: Option<&EditorState>,
    target: Option<&MaskId>,
) -> Vec<(char, String)> {
    let kind = source_kind(state);
    modules
        .iter()
        .filter(|module| module.is_available() && applies(module, state))
        .filter_map(|module| {
            let letter = module.canvas.as_ref()?.shortcut()?.chars().next()?;
            let mode = picker_mode(modules, module, kind, target).unwrap_or(&module.id);
            Some((letter, mode.to_owned()))
        })
        .collect()
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

/// What a click on the photograph does in one canvas mode, as that mode's module — or the host —
/// declares it.
///
/// A pick belongs to the mode the session is in, never to "whichever module declares one first":
/// several modules declare a canvas pick, and only the one whose canvas is on screen may answer
/// for a click. A crop frame is a different adapter and is not a pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasPick<'a> {
    /// Fill these coordinate parameters of an action with the located content pixel. It commits
    /// only when the module declares `commit`: then the two coordinates are the whole request and
    /// the pick submits it; otherwise the person submits the action themselves.
    Point {
        action: &'a str,
        x: &'a str,
        y: &'a str,
        commit: bool,
    },
    /// Run this module query at the located content pixel and submit the fields it answers with to
    /// the module action once. A refused query commits nothing and its reason is shown.
    Sample {
        query: &'a str,
        x: &'a str,
        y: &'a str,
        action: &'a str,
    },
    /// The same interaction with the **host's** own pair: a `mask.*` read answers the pixel and a
    /// `mask.*` command receives it. A mask is a host object and no module declares one, so a pick
    /// that fills part of a mask reaches the host's declarations instead of a module's; everything
    /// else about it — the query first, the numeric fields the action declares, the refusal that
    /// commits nothing — is identical, which is why it is the same enum and not a second mechanism.
    HostSample {
        query: &'a str,
        x: &'a str,
        y: &'a str,
        action: &'a str,
    },
}

/// The pick the active canvas mode declares, if that mode declares one at all.
///
/// The host's own picks are consulted first and by the same key: a pick's mode is its action's method
/// name, which carries a dot and therefore can never be a module id.
pub(crate) fn canvas_pick<'a>(
    modules: &'a [ModuleDescriptor],
    mode: &str,
) -> Option<CanvasPick<'a>> {
    if let Some(CanvasInteraction::SampleApply {
        query,
        x,
        y,
        action,
        ..
    }) = luxforge_core::mask::commands::canvas_pick(mode)
    {
        return Some(CanvasPick::HostSample {
            query: query.as_str(),
            x: x.as_str(),
            y: y.as_str(),
            action: action.as_str(),
        });
    }
    let module = module_of(modules, mode).filter(|module| module.is_available())?;
    match module.canvas.as_ref()? {
        CanvasInteraction::PointPick {
            action,
            x,
            y,
            commit,
            ..
        } => Some(CanvasPick::Point {
            action: action.as_str(),
            x: x.as_str(),
            y: y.as_str(),
            commit: *commit,
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

    /// The frame a `recipe.describe` row's values hold, read under the declared parameter names:
    /// the inverse of [`Self::params`]. `None` when a value is missing or not a number, as it is
    /// for a row whose provider could not read its payload.
    pub(crate) fn payload(&self, values: &Map<String, Value>) -> Option<CropPayload> {
        let read = |name: &str| values.get(name).and_then(Value::as_f64);
        Some(CropPayload {
            angle: read(self.angle)?,
            x: read(self.x)?,
            y: read(self.y)?,
            width: read(self.width)?,
            height: read(self.height)?,
        })
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

/// The row of the crop layer a draft edits, and its index: the stack's first layer of the frame's
/// geometry effect.
pub(crate) fn crop_row<'a>(
    frame: &CropFrame<'_>,
    recipe: &'a RecipeDescription,
) -> Option<(usize, &'a LayerDescription)> {
    let effect = frame.effect()?;
    recipe
        .layers
        .iter()
        .enumerate()
        .find(|(_, row)| row.effect == effect)
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
///
/// Controls resolve for the open photo and the bound target exactly as the panel draws them, so an
/// entry runs what its button would: on a RAW photo's global target Basic's As shot is the RAW
/// development's. A pick mode is listed only when a resolved picker names it ([`pick_modes`]).
pub(crate) fn palette_entries(
    modules: &[ModuleDescriptor],
    developer: bool,
    state: Option<&EditorState>,
    target: Option<&MaskId>,
) -> Vec<(String, String, PaletteAction)> {
    let kind = source_kind(state);
    let picks = pick_modes(modules, state, target);
    let mut entries = Vec::new();
    for module in modules
        .iter()
        .filter(|module| module.is_available())
        .filter(|module| !module.developer || developer)
    {
        collect_actions(modules, module, kind, target, &mut entries);
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
            let reachable = match canvas {
                CanvasInteraction::CropFrame { .. } => true,
                CanvasInteraction::PointPick { .. } | CanvasInteraction::SampleApply { .. } => {
                    picks.contains(&module.id.as_str())
                }
            };
            if reachable {
                entries.push((
                    format!("Mode · {}", canvas.title()),
                    "workspace.set".to_owned(),
                    PaletteAction::Mode(module.id.clone()),
                ));
            }
        }
    }
    entries
}

fn collect_actions(
    modules: &[ModuleDescriptor],
    module: &ModuleDescriptor,
    kind: Option<SourceTag>,
    target: Option<&MaskId>,
    entries: &mut Vec<(String, String, PaletteAction)>,
) {
    for declared in walk(&module.controls) {
        let Some((_, control)) = resolved(modules, module, declared, kind, target) else {
            continue;
        };
        if let Control::Action(button) = control {
            entries.push((
                format!("{} · {}", module.title, button.label),
                format!("edit.{}", button.action),
                PaletteAction::Run {
                    action: button.action.clone(),
                    preset: button.preset.clone(),
                },
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testing::{CROP_ASPECTS, CROP_EFFECT, crop_descriptor};
    use luxforge_core::Availability;
    use serde_json::json;

    #[test]
    fn a_canvas_mode_routes_only_to_its_own_declared_pick() {
        let modules: Vec<_> = luxforge_core::ModuleRegistry::developer()
            .descriptors()
            .into_iter()
            .cloned()
            .collect();
        // The RAW sensor picker and the pixel proof both declare a plain point pick, and each
        // answers only for its own mode. RAW's declares that the pick commits; the proof's fills.
        assert_eq!(
            canvas_pick(&modules, "luxforge.raw"),
            Some(CanvasPick::Point {
                action: "pick-raw-neutral",
                x: "x",
                y: "y",
                commit: true,
            })
        );
        assert_eq!(
            canvas_pick(&modules, "luxforge.pixel"),
            Some(CanvasPick::Point {
                action: "set-pixel",
                x: "x",
                y: "y",
                commit: false,
            })
        );
        // Basic's neutral picker is a sample-apply pick: a query first, then one command.
        assert_eq!(
            canvas_pick(&modules, "luxforge.basic"),
            Some(CanvasPick::Sample {
                query: "neutral-sample",
                x: "x",
                y: "y",
                action: "set-basic"
            })
        );
        // A crop frame is a different adapter, and the pointer mode names no module at all.
        assert!(canvas_pick(&modules, "luxforge.crop").is_none());
        assert!(canvas_pick(&modules, luxforge_core::POINTER_MODE).is_none());
    }

    /// Which generated sliders draft. The rule is about the request, not the module: one field is a
    /// whole request when the action merges it or declares nothing else, and only then.
    #[test]
    fn a_slider_drafts_for_a_patch_field_or_an_actions_only_parameter() {
        let modules: Vec<_> = luxforge_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect();
        // An action of one parameter is a complete request on its own: RAW's explicit gains.
        for (action, parameter) in [("set-raw-red-gain", "gain"), ("set-raw-blue-gain", "gain")] {
            assert!(
                drafts_alone(&modules, action, parameter),
                "{action}.{parameter} declares no second parameter"
            );
            assert!(!is_patch(&modules, action), "{action} is not a field patch");
            assert!(drafts(&modules, action, parameter));
        }
        // RAW's white balance is a patch, which drafts for that reason.
        assert!(is_patch(&modules, "set-raw"));
        assert!(drafts(&modules, "set-raw", "temperature"));
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
        assert!(!drafts(&modules, "set-raw-red-gain", "kelvin"));
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

    /// A control's menu target carries the method its copied request is published as, read from
    /// the host's command table when the menu opens: `edit.<action>` for a module action, and the
    /// command itself for a host `mask.*` command.
    #[test]
    fn a_control_menu_target_carries_its_published_method() {
        let method = |action: &str| match MenuTarget::control(action.to_owned(), None, None) {
            MenuTarget::Control { method, .. } => method,
            other => panic!("{other:?} is not a control's menu"),
        };
        assert_eq!(method("basic"), "edit.basic");
        let command = luxforge_core::mask::commands::all()
            .first()
            .expect("the host has mask commands")
            .method;
        assert!(command.starts_with("mask."), "{command}");
        assert_eq!(method(command), command);
    }
}
