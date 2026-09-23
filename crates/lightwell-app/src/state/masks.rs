//! The Masks panel model: the mask list, the open mask's component list, and the host controls the
//! two are edited through.
//!
//! Nothing is invented here. Every value comes from `mask.list`, every control is one the host
//! declares in [`lightwell_core::mask::commands::controls`], and every button that would be refused
//! by the command family carries that family's own reason instead of being offered — the design's
//! rule that the panel surfaces a refusal rather than presenting a button the host will reject.
//!
//! The adjustments that apply *through* a mask are not modelled here at all: they are the delivered
//! generated sections of the maskable modules, derived by [`super::tools`] with this panel's
//! selected mask as their target.
use crate::{
    mask_draft::MaskDraft,
    state::{
        Inputs,
        tools::{ControlModel, ControlOwner, Rendered, classify, control_model},
    },
};
use lightwell_core::{
    COMPONENTS_PER_MASK, ComponentId, ComponentMode, MASKS_PER_RECIPE, MaskId, MaskOverlayColour,
    MaskOverlayMode,
    mask::commands::{ComponentReport, MaskReport},
};

/// One mask's row in the list.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MaskRow {
    pub(crate) id: MaskId,
    /// Position in `recipe.masks`, which is the order overlapping masks apply in.
    pub(crate) index: usize,
    pub(crate) name: String,
    /// The amount readout, already formatted with its unit.
    pub(crate) amount: String,
    /// This mask changes the picture: at least one layer is bound to it and its amount is not zero.
    /// It is the panel's non-neutral dot, derived rather than stored, exactly as a module section's
    /// own dot is.
    pub(crate) non_neutral: bool,
    pub(crate) inverted: bool,
    /// The eye: per-client view state that chooses whether the overlay draws this mask. It commits
    /// nothing and changes no render — a mask always applies to the picture whether or not its
    /// overlay is drawn, because hiding an edit and hiding its indicator are different things.
    pub(crate) visible: bool,
    pub(crate) selected: bool,
    /// The layers bound to this mask, by the title their provider gives them.
    pub(crate) layers: Vec<String>,
    /// A component of a kind this build cannot evaluate is retained and reported; the row says so
    /// rather than drawing the mask as if it were complete.
    pub(crate) unavailable: Option<String>,
    pub(crate) can_move_up: bool,
    pub(crate) can_move_down: bool,
}

/// One component's row inside the open mask.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ComponentRow {
    pub(crate) id: ComponentId,
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) kind: String,
    /// The declared mode token — `add`, `subtract`, `intersect` — as the host spells it. The view
    /// shows it and names no vocabulary of its own.
    pub(crate) mode: String,
    pub(crate) inverted: bool,
    /// This build can evaluate the kind. A component it cannot is kept, listed and named, and every
    /// edit to it is refused rather than silently dropped.
    pub(crate) available: bool,
    pub(crate) selected: bool,
    /// The three-way mode control and the invert toggle, generated from the host's declarations.
    /// Empty for the first component, whose mode is fixed by the composition.
    pub(crate) controls: Vec<ControlModel>,
    /// The kind's own number fields, shown beneath the row while it is selected, so no gesture is
    /// reachable only by pointer.
    pub(crate) fields: Vec<ControlModel>,
    /// Why this component's mode cannot be changed, when it cannot: the first component of a mask is
    /// always `add`, because nothing precedes it to subtract from.
    pub(crate) mode_reason: Option<String>,
    /// Why Delete is refused, when it is. A mask never exists empty, so its only component is
    /// deleted by deleting the mask; the panel offers that instead.
    pub(crate) delete_reason: Option<String>,
    /// Why this row cannot move up or down, when it cannot: an order that would leave a non-`add`
    /// component leading is refused by the command family.
    pub(crate) up_reason: Option<String>,
    pub(crate) down_reason: Option<String>,
    /// This row can be edited on the canvas: its kind has a handle editor in this build.
    pub(crate) can_edit_shape: bool,
}

impl ComponentRow {
    pub(crate) fn can_move_up(&self) -> bool {
        self.up_reason.is_none()
    }

    pub(crate) fn can_move_down(&self) -> bool {
        self.down_reason.is_none()
    }
}

/// One registered component kind offered by New mask and by the Add row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KindOption {
    pub(crate) kind: String,
    pub(crate) label: String,
    /// This kind has a handle editor in this build. A registered kind without one is still listed,
    /// because its components are still editable through their number fields, and saying so is
    /// honest where hiding it would not be.
    pub(crate) drawable: bool,
    pub(crate) enabled: bool,
}

/// One declared geometry field of the open gesture, as the panel offers it.
///
/// Every handle has a number field, so no gesture is reachable only by pointer. While a gesture is
/// open the field nudges the draft itself — which is what makes the canvas follow it exactly as it
/// follows the pointer — rather than committing an edit of its own.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DraftField {
    /// The declared parameter's name, which is what the request carries.
    pub(crate) name: String,
    /// Its label, as the host's own control declares it.
    pub(crate) label: String,
    /// The value, formatted to the declared precision.
    pub(crate) text: String,
    pub(crate) value: f64,
    /// The declared step one nudge moves by.
    pub(crate) step: f64,
}

/// The open shape gesture, as the panel and the draft bar read it.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MaskDraftModel {
    /// The gesture's own declared fields, each editable as a number.
    pub(crate) fields: Vec<DraftField>,
    /// What releasing it commits, in words: `New mask`, `Subtract`, `Update`.
    pub(crate) title: String,
    /// The method the commit calls, which is what Copy as JSON request copies.
    pub(crate) method: String,
    pub(crate) kind: String,
    /// The gesture's own numbers, each a declared field name and its formatted value.
    pub(crate) readout: Vec<(String, String)>,
    pub(crate) conflicted: bool,
    pub(crate) can_apply: bool,
    pub(crate) apply_reason: Option<String>,
}

/// What the canvas draws of the selected mask, as the panel's overlay control reads it.
///
/// The options are the host's own vocabulary, already spelled: the view renders the strings and
/// publishes the index that was chosen, so it names no mode and no colour of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayModel {
    pub(crate) modes: Vec<String>,
    pub(crate) selected: usize,
    pub(crate) colours: Vec<String>,
    pub(crate) colour_selected: usize,
    /// The chosen mode is the tinted one, so the colour control applies.
    pub(crate) tinting: bool,
    /// The overlay is showing something: the mode is not `off` and a mask is selected.
    pub(crate) on: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MasksModel {
    pub(crate) masks: Vec<MaskRow>,
    /// What the list says in place of rows.
    pub(crate) caption: Option<String>,
    pub(crate) selected: Option<MaskId>,
    /// The open mask's components, in the order they compose.
    pub(crate) components: Vec<ComponentRow>,
    /// The whole-mask amount and inversion, generated from the host's own declarations.
    pub(crate) controls: Vec<ControlModel>,
    /// The kinds New mask offers, and the kinds the Add row offers under an open mask.
    pub(crate) kinds: Vec<KindOption>,
    /// The three modes a new component can take, in the order the host declares them, and the one
    /// the next Add gesture will use — chosen before the gesture rather than guessed from a modifier
    /// afterwards.
    pub(crate) modes: Vec<String>,
    pub(crate) add_mode: usize,
    /// Why nothing here can be run right now, in the words the status bar uses.
    pub(crate) disabled_reason: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) draft: Option<MaskDraftModel>,
    pub(crate) overlay: OverlayModel,
    /// Why a new mask cannot be created, when the recipe is full.
    pub(crate) create_reason: Option<String>,
    /// Why a component cannot be added to the open mask, when it is full.
    pub(crate) add_reason: Option<String>,
    /// The open mask's name as it is being typed. A rename needs free text, which no declared
    /// parameter kind carries, so the name travels in the request's envelope and this is where the
    /// panel keeps what has been typed for it.
    pub(crate) name: String,
}

impl Default for MasksModel {
    fn default() -> Self {
        Self {
            masks: Vec::new(),
            caption: None,
            selected: None,
            components: Vec::new(),
            controls: Vec::new(),
            kinds: Vec::new(),
            modes: Vec::new(),
            // The mode a new component takes unless one is chosen: the only mode a mask's first
            // component may have, so the default is never the one the host refuses.
            add_mode: 0,
            disabled_reason: None,
            enabled: false,
            draft: None,
            overlay: OverlayModel {
                modes: Vec::new(),
                selected: 0,
                colours: Vec::new(),
                colour_selected: 0,
                tinting: false,
                on: false,
            },
            create_reason: None,
            add_reason: None,
            name: String::new(),
        }
    }
}

impl MasksModel {
    /// Correlated evidence: what the panel showed when a frame was captured.
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "masks": self.masks.iter().map(|row| serde_json::json!({
                "id": row.id.as_str(),
                "index": row.index,
                "name": row.name,
                "amount": row.amount,
                "non_neutral": row.non_neutral,
                "inverted": row.inverted,
                "visible": row.visible,
                "selected": row.selected,
                "layers": row.layers,
                "unavailable": row.unavailable,
            })).collect::<Vec<_>>(),
            "selected": self.selected.as_ref().map(MaskId::as_str),
            "components": self.components.iter().map(|row| serde_json::json!({
                "id": row.id.as_str(),
                "index": row.index,
                "name": row.name,
                "kind": row.kind,
                "mode": row.mode.as_str(),
                "inverted": row.inverted,
                "available": row.available,
                "selected": row.selected,
                "delete_reason": row.delete_reason,
                "mode_reason": row.mode_reason,
            })).collect::<Vec<_>>(),
            "kinds": self.kinds.iter().map(|kind| kind.kind.clone()).collect::<Vec<_>>(),
            "add_mode": self.modes.get(self.add_mode),
            "overlay": self.overlay.modes.get(self.overlay.selected),
            "overlay_colour": self.overlay.colours.get(self.overlay.colour_selected),
            "draft": self.draft.as_ref().map(|draft| serde_json::json!({
                "title": draft.title,
                "method": draft.method,
                "conflicted": draft.conflicted,
                "readout": draft.readout,
            })),
            "name": self.name,
            "enabled": self.enabled,
            "disabled_reason": self.disabled_reason,
        })
    }
}

/// The three modes a component can take, in the order the host's own enum declares them. One list,
/// read by the model that offers them and by the controller that resolves a chosen index.
pub(crate) const MODES: [ComponentMode; 3] = [
    ComponentMode::Add,
    ComponentMode::Subtract,
    ComponentMode::Intersect,
];

/// One mode's position in that list.
pub(crate) fn mode_index(mode: ComponentMode) -> usize {
    MODES.iter().position(|known| *known == mode).unwrap_or(0)
}

/// The overlay control's options and the two selections it shows.
fn overlay_model(inputs: &Inputs<'_>) -> OverlayModel {
    let workspace = &inputs.session.workspace;
    OverlayModel {
        modes: MaskOverlayMode::ALL
            .iter()
            .map(|mode| mode.as_str().to_owned())
            .collect(),
        selected: MaskOverlayMode::ALL
            .iter()
            .position(|mode| *mode == workspace.mask_overlay)
            .unwrap_or(0),
        colours: MaskOverlayColour::ALL
            .iter()
            .map(|colour| colour.as_str().to_owned())
            .collect(),
        colour_selected: MaskOverlayColour::ALL
            .iter()
            .position(|colour| *colour == workspace.mask_overlay_colour)
            .unwrap_or(0),
        tinting: workspace.mask_overlay == MaskOverlayMode::Tint,
        on: workspace.mask_overlay != MaskOverlayMode::Off && inputs.selected_mask.is_some(),
    }
}

/// The Masks panel for the displayed entry.
pub(crate) fn derive(inputs: &Inputs<'_>) -> MasksModel {
    let disabled_reason = disabled_reason(inputs);
    let enabled = disabled_reason.is_none();
    let listing = inputs
        .masks
        .filter(|listing| Some(&listing.entry_id) == inputs.display_entry);
    let reports: &[MaskReport] = listing
        .map(|listing| listing.masks.as_slice())
        .unwrap_or(&[]);
    // A selection that the stack no longer holds — undone away, deleted by another client — is
    // dropped rather than left pointing at nothing.
    let selected = inputs
        .selected_mask
        .filter(|id| reports.iter().any(|report| &&report.id == id))
        .cloned();
    let masks: Vec<MaskRow> = reports
        .iter()
        .map(|report| MaskRow {
            id: report.id.clone(),
            index: report.index,
            name: report.name.clone(),
            amount: format!("{:.0}%", report.amount),
            // A mask with no layer bound to it changes nothing yet, and neither does one turned all
            // the way down: the dot says "this is doing something", not "this exists".
            non_neutral: !report.layers.is_empty() && report.amount > 0.0,
            inverted: report.invert,
            visible: !inputs.hidden_masks.contains(&report.id),
            selected: selected.as_ref() == Some(&report.id),
            layers: report
                .layers
                .iter()
                .map(|layer| layer.title.clone().unwrap_or_else(|| layer.effect.clone()))
                .collect(),
            unavailable: unavailable(&report.components),
            can_move_up: enabled && report.index > 0,
            can_move_down: enabled && report.index + 1 < reports.len(),
        })
        .collect();
    let open = selected
        .as_ref()
        .and_then(|id| reports.iter().find(|report| &report.id == id));
    let components = open
        .map(|report| component_rows(report, inputs, enabled))
        .unwrap_or_default();
    MasksModel {
        caption: caption(inputs, listing.is_some(), reports.is_empty()),
        create_reason: (reports.len() >= MASKS_PER_RECIPE)
            .then(|| format!("This recipe holds {MASKS_PER_RECIPE} masks, which is the limit")),
        add_reason: open.and_then(|report| {
            (report.components.len() >= COMPONENTS_PER_MASK).then(|| {
                format!(
                    "{} holds {COMPONENTS_PER_MASK} components, which is the limit",
                    report.name
                )
            })
        }),
        masks,
        selected,
        controls: mask_controls(inputs, enabled && open.is_some()),
        components,
        kinds: kinds(enabled),
        modes: MODES.iter().map(|mode| mode.as_str().to_owned()).collect(),
        add_mode: mode_index(inputs.mask_mode),
        disabled_reason,
        enabled,
        draft: draft_model(inputs, enabled),
        name: inputs.mask_name.to_owned(),
        overlay: overlay_model(inputs),
    }
}

/// Why nothing in the panel can run, in the words the status bar uses. The same rules a module
/// section follows, because a mask command is the same kind of mutation.
fn disabled_reason(inputs: &Inputs<'_>) -> Option<String> {
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

fn caption(inputs: &Inputs<'_>, listed: bool, empty: bool) -> Option<String> {
    if inputs.state.is_none() {
        return Some("No photograph is open".into());
    }
    if !listed {
        return Some("Reading masks…".into());
    }
    empty.then(|| "No masks yet · New mask draws one on the photograph".to_owned())
}

/// The first component kind this mask holds that this build cannot evaluate, named. A retained kind
/// is reported everywhere rather than hidden, exactly as a layer with no provider is.
fn unavailable(components: &[ComponentReport]) -> Option<String> {
    components
        .iter()
        .find(|component| !component.available)
        .map(|component| format!("unknown mask component {}", component.kind))
}

/// Every kind the Add row and New mask can actually create, as they offer it. That is the kinds
/// whose geometry is declared as numbers, not every kind the build can evaluate: a drawn kind like
/// the brush is parsed, evaluated and retained but generates no `mask.create-<kind>`, so offering it
/// here would put up a button with no command behind it. The list is still the host's, so
/// registering a kind with declared geometry is what puts it here.
fn kinds(enabled: bool) -> Vec<KindOption> {
    lightwell_core::mask::declared_geometry_kinds()
        .map(|kind| KindOption {
            kind: kind.to_owned(),
            label: lightwell_core::mask::kind_title(kind),
            drawable: drawable(kind),
            enabled,
        })
        .collect()
}

/// This kind has a canvas handle editor in this build. Only the linear gradient does today; a
/// component of another registered kind is created and edited through its declared number fields.
fn drawable(kind: &str) -> bool {
    kind == crate::mask_draft::LINEAR
}

/// The whole-mask controls: the amount slider and the inversion toggle, generated from the host's
/// own declarations exactly as a module's are.
fn mask_controls(inputs: &Inputs<'_>, enabled: bool) -> Vec<ControlModel> {
    host_controls(inputs, enabled, |action| {
        action == "mask.set-amount" || action == "mask.set-invert"
    })
}

/// The host controls whose action `wanted` accepts, in declared order, modelled through the same
/// generic path a module's controls take.
fn host_controls(
    inputs: &Inputs<'_>,
    enabled: bool,
    wanted: impl Fn(&str) -> bool,
) -> Vec<ControlModel> {
    lightwell_core::mask::commands::controls()
        .iter()
        .enumerate()
        .filter(|(_, control)| control_action(control).is_some_and(&wanted))
        .map(|(index, control)| {
            control_model(ControlOwner::Host, control, inputs, enabled, &[index])
        })
        .collect()
}

/// The action one declared control submits, for the filters above. A control kind that submits none
/// belongs to no mask command and is never offered here.
fn control_action(control: &lightwell_core::Control) -> Option<&str> {
    match classify(control) {
        Rendered::Number { action, .. }
        | Rendered::Toggle { action, .. }
        | Rendered::Choice { action, .. }
        | Rendered::Color { action, .. }
        | Rendered::Curve { action, .. }
        | Rendered::Action { action, .. }
        | Rendered::Presets { action } => Some(action),
        Rendered::Group { .. } | Rendered::Picker { .. } | Rendered::Unsupported(_) => None,
    }
}

/// The open mask's component list, with each row's refusals resolved from the command family's own
/// rules rather than discovered by sending a request that will be rejected.
fn component_rows(report: &MaskReport, inputs: &Inputs<'_>, enabled: bool) -> Vec<ComponentRow> {
    let only = report.components.len() == 1;
    report
        .components
        .iter()
        .map(|component| {
            let first = component.index == 0;
            let selected = inputs.selected_component == Some(&component.id);
            // A mask's first component is always `add`: nothing precedes it to subtract from or
            // intersect with, so the mode control is not offered rather than offered and refused.
            let mode_reason = first.then(|| {
                format!(
                    "{} is the first component of {}, and a mask's first component is always add",
                    component.name, report.name
                )
            });
            // A mask never exists empty, so its last component is removed by removing the mask.
            let delete_reason = only.then(|| {
                format!(
                    "{} is the only component of {}; delete the mask instead",
                    component.name, report.name
                )
            });
            ComponentRow {
                // Moving a row into or out of the leading position is refused whenever it would
                // leave a component that is not an add at the front, which is the same rule the
                // host checks; the panel states it here instead of offering the move.
                up_reason: move_reason(report, component.index, -1, enabled),
                down_reason: move_reason(report, component.index, 1, enabled),
                controls: if first {
                    Vec::new()
                } else {
                    host_controls(inputs, enabled && component.available, |action| {
                        action == "mask.set-component-mode" || action == "mask.set-component-invert"
                    })
                },
                fields: if selected {
                    kind_fields(inputs, &component.kind, enabled && component.available)
                } else {
                    Vec::new()
                },
                id: component.id.clone(),
                index: component.index,
                name: component.name.clone(),
                kind: component.kind.clone(),
                mode: component.mode.as_str().to_owned(),
                inverted: component.invert,
                available: component.available,
                selected,
                mode_reason,
                delete_reason,
                can_edit_shape: enabled && component.available && drawable(&component.kind),
            }
        })
        .collect()
}

/// Why one component cannot move by `step` places, or `None` when it can.
fn move_reason(report: &MaskReport, index: usize, step: i64, enabled: bool) -> Option<String> {
    if !enabled {
        return Some("Waiting for the last request".into());
    }
    let target = index as i64 + step;
    if target < 0 || target >= report.components.len() as i64 {
        return Some(format!("{} is already at the end of the list", report.name));
    }
    let target = target as usize;
    // Only a move that touches the leading position can break the composition's one structural
    // rule: whichever component ends up first must be an add.
    let leading = if index == 0 {
        report.components.get(1)
    } else if target == 0 {
        report.components.get(index)
    } else {
        return None;
    };
    leading
        .filter(|component| component.mode != ComponentMode::Add)
        .map(|component| {
            format!(
                "that would leave {} leading, and a mask's first component is always add",
                component.name
            )
        })
}

/// One kind's declared geometry fields, which are the host controls whose action is that kind's own
/// patch method. The panel names no field: registering a kind brings its fields with it.
fn kind_fields(inputs: &Inputs<'_>, kind: &str, enabled: bool) -> Vec<ControlModel> {
    let Some(command) = lightwell_core::mask::commands::geometry(
        lightwell_core::mask::commands::GeometryOp::Set,
        kind,
    ) else {
        return Vec::new();
    };
    host_controls(inputs, enabled, |action| action == command.method)
}

fn draft_model(inputs: &Inputs<'_>, enabled: bool) -> Option<MaskDraftModel> {
    let draft: &MaskDraft = inputs.mask_draft?;
    let apply_reason = if draft.conflicted {
        Some("Changed elsewhere: discard the draft or reapply it".into())
    } else if !enabled {
        disabled_reason(inputs)
    } else if draft.method().is_none() {
        Some(format!("This build cannot draw a {} component", draft.kind))
    } else {
        None
    };
    // The fields the open gesture offers, from the same declarations its commit is validated
    // against: the panel names no field and no step of its own.
    let patch = lightwell_core::mask::commands::geometry(
        lightwell_core::mask::commands::GeometryOp::Set,
        &draft.kind,
    );
    let fields = draft
        .values()
        .into_iter()
        .map(|(name, value)| {
            let declared = patch.and_then(|command| command.action.parameter(name));
            DraftField {
                name: name.to_owned(),
                label: crate::state::tools::labelled_control(
                    lightwell_core::mask::commands::controls(),
                    patch.map(|command| command.method).unwrap_or_default(),
                    name,
                )
                .unwrap_or(name)
                .to_owned(),
                text: declared
                    .map(|declared| crate::app::fields::format_number(declared, value))
                    .unwrap_or_else(|| format!("{value:.4}")),
                value,
                step: declared
                    .and_then(|declared| declared.step)
                    .filter(|step| step.is_finite() && *step > 0.0)
                    .unwrap_or(0.01),
            }
        })
        .collect();
    Some(MaskDraftModel {
        fields,
        title: draft.op.label().to_owned(),
        method: draft.method().unwrap_or_default().to_owned(),
        kind: draft.kind.clone(),
        readout: draft
            .values()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), format!("{value:.4}")))
            .collect(),
        conflicted: draft.conflicted,
        can_apply: apply_reason.is_none(),
        apply_reason,
    })
}
