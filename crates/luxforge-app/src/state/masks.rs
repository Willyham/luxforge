//! The Masks panel model: the mask list, the open mask's component list, and the host controls the
//! two are edited through.
//!
//! Nothing is invented here. Every value comes from `mask.list`, every control is one the host
//! declares in [`luxforge_core::mask::commands::controls`], and every button that would be refused
//! by the command family carries that family's own reason instead of being offered — the design's
//! rule that the panel surfaces a refusal rather than presenting a button the host will reject. The
//! reasons are not re-derived here: each is the refusal [`luxforge_core::mask::rules`] returns for
//! the facts the listing reports, which is the same function the command refuses through, so the
//! limits, the leading-add rule and their wording live in the host alone.
//!
//! The adjustments that apply *through* a mask are not modelled here at all: they are the delivered
//! generated sections of the maskable modules, derived by [`super::tools`] with this panel's
//! selected mask as their target.
use crate::{
    mask_draft::MaskDraft,
    state::{
        Inputs,
        number::NumberSpec,
        tools::{ControlModel, ControlOwner, Rendered, classify, control_model},
    },
};
use luxforge_core::{
    ComponentId, ComponentMode, MaskId, MaskOverlayColour, MaskOverlayMode,
    mask::{
        commands::{self, ComponentReport, MaskReport, SampleOp},
        rules,
    },
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
    /// The kind's own display title, as the host gives it, so the row says what it is rather than
    /// relying on the ordinal in its name.
    pub(crate) kind_title: String,
    /// The declared mode token — `add`, `subtract`, `intersect` — as the host spells it. The view
    /// shows it and names no vocabulary of its own.
    pub(crate) mode: String,
    pub(crate) inverted: bool,
    /// This build can evaluate the kind. A component it cannot is kept, listed and named, and every
    /// edit to it is refused rather than silently dropped.
    pub(crate) available: bool,
    pub(crate) selected: bool,
    /// The pointer is over this row, so the overlay is showing this component alone.
    pub(crate) hovered: bool,
    /// This row's own three-way mode control: the options the host's `mode` parameter declares and
    /// **this** component's mode among them. It is the row's, not the panel's — changing row three's
    /// mode never edits row one, and the mode is a property of a component rather than a decision
    /// frozen when it was created.
    pub(crate) mode_options: Vec<String>,
    pub(crate) mode_selected: usize,
    pub(crate) mode_label: String,
    /// This row's own invert toggle, labelled as the host's control declares it.
    pub(crate) invert_label: String,
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
    /// This component's geometry is painted, so the canvas gesture for it is another stroke rather
    /// than a handle drag, and the row says so.
    pub(crate) painted: bool,
    /// The strokes this component holds, in the order they compose, shown while the row is selected
    /// so every stroke is a thing a person can see and remove. Empty for a component that holds
    /// none, which is every component whose geometry is declared as numbers.
    pub(crate) strokes: Vec<StrokeRow>,
    /// The colours this component has sampled, in the order it holds them, shown while the row is
    /// selected so every swatch is a thing a person can see and remove one at a time. Empty for a
    /// kind that samples nothing.
    pub(crate) samples: Vec<SampleRow>,
    /// This component's kind samples colours from the photograph, so the panel offers the host's own
    /// canvas pick for it.
    pub(crate) can_pick: bool,
    /// The canvas pick is on, so a click on the photograph adds a swatch.
    pub(crate) picking: bool,
    /// What the pick button reads, from the host's own declaration.
    pub(crate) pick_label: String,
    /// Why a colour cannot be picked into this component right now.
    pub(crate) pick_reason: Option<String>,
    /// What this component's kind does **not** select, shown on the open row in the kind's own terms.
    ///
    /// Empty for a position-based kind, which has none of these limits: a gradient and a brush select
    /// where they are drawn and nothing about the picture's values changes that. A value-based kind
    /// carries two lines — what its one axis cannot tell apart, and what reading the operation's input
    /// costs — because both are surprising and both are measured
    /// (`docs/design/range-study.md`), and a person who learns them from a rendered frame instead has
    /// already made an edit they did not mean. It is read from
    /// [`luxforge_core::mask::component_kind_limits`], which is the host's own kind table, so a kind
    /// registered later carries its own line and the shared one without this being edited.
    pub(crate) limits: Vec<String>,
}

/// One sampled colour of a component that holds a list of them, as the panel lists it.
///
/// The swatch is the stored linear triple shown as the 8-bit codes a person can read; the panel
/// converts nothing else and invents nothing — the numbers are the ones `mask.list` reports.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SampleRow {
    pub(crate) index: usize,
    /// What the row reads: `Colour 1`.
    pub(crate) label: String,
    /// The stored linear-sRGB triple, formatted for the readout.
    pub(crate) text: String,
    /// The swatch as 8-bit sRGB, for the colour chip beside it.
    pub(crate) swatch: [u8; 3],
    pub(crate) delete_reason: Option<String>,
}

/// One stroke of a brush component, as the panel lists it.
///
/// A stroke is an object and not an event: it has a content address, a place in the order its
/// component composes in, and a delete of its own. **That delete is a forward edit** — it appends an
/// entry and removes only that stroke, leaving everything committed after it exactly where it is —
/// which is a different thing from undo, and the row says so rather than leaving the two to look
/// alike.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StrokeRow {
    /// The content address, which is what the delete addresses it by.
    pub(crate) stroke: String,
    pub(crate) index: usize,
    /// What the row reads: `Stroke 1`, and its position in the fold.
    pub(crate) label: String,
    /// Why this stroke cannot be removed, when it cannot: a component with no stroke covers nothing,
    /// so its last stroke goes by removing the component.
    pub(crate) delete_reason: Option<String>,
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
    /// This kind is created by a button rather than by a gesture, because every field its geometry
    /// declares carries a default. A range selection is *typed*: there is nothing to drag, so it is
    /// created as the starting selection its defaults describe and narrowed afterwards through the
    /// number fields the same declarations generate.
    pub(crate) typed: bool,
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
    /// This gesture is painted, so its numbers are the brush's own and the Brush section already
    /// offers them. The panel shows one set of fields rather than two identical ones.
    pub(crate) painted: bool,
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
    /// The brush the next stroke will be drawn with, and what it can be put down on. It is its own
    /// section rather than a button in the Add row: the Add row offers the kinds that declare their
    /// geometry as numbers, and a brush declares none, so a Brush button there would be a button
    /// with no command behind it.
    pub(crate) brush: BrushModel,
    pub(crate) overlay: OverlayModel,
    /// Why a new mask cannot be created, when the recipe is full.
    pub(crate) create_reason: Option<String>,
    /// Why a component cannot be added to the open mask, when it is full.
    pub(crate) add_reason: Option<String>,
    /// The open mask's name as it is being typed: the text `mask.rename` takes as its declared
    /// `name` parameter, kept here until it is sent.
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
            brush: BrushModel {
                fields: Vec::new(),
                erase: false,
                erase_label: String::new(),
                limit: false,
                limit_label: String::new(),
                limit_reason: None,
                erase_held: false,
                locked: false,
                armed: false,
                can_add: false,
                enabled: false,
            },
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
                "hovered": row.hovered,
                "mode_options": row.mode_options,
                "delete_reason": row.delete_reason,
                "mode_reason": row.mode_reason,
                "up_reason": row.up_reason,
                "down_reason": row.down_reason,
                "painted": row.painted,
                "strokes": row.strokes.iter().map(|stroke| serde_json::json!({
                    "stroke": stroke.stroke,
                    "index": stroke.index,
                    "delete_reason": stroke.delete_reason,
                })).collect::<Vec<_>>(),
                "samples": row.samples.iter().map(|sample| serde_json::json!({
                    "index": sample.index,
                    "text": sample.text,
                    "swatch": sample.swatch,
                })).collect::<Vec<_>>(),
                "picking": row.picking,
                "pick_reason": row.pick_reason,
                // The kind's own number fields as the open row shows them, so a captured frame is
                // evidence of the geometry a person can read rather than of the payload behind it,
                // and the statement above them travels with it. Both are empty on a closed row,
                // exactly as the panel draws them.
                "fields": row.fields.iter().filter_map(|field| match field {
                    crate::state::tools::ControlModel::Slider(slider) =>
                        Some((slider.parameter.clone(), serde_json::json!(slider.value))),
                    _ => None,
                }).collect::<serde_json::Map<_, _>>(),
                "limits": row.limits,
            })).collect::<Vec<_>>(),
            "brush": serde_json::json!({
                "fields": self.brush.fields.iter()
                    .map(|field| (field.name.clone(), serde_json::json!(field.value)))
                    .collect::<serde_json::Map<_, _>>(),
                "erase": self.brush.erase,
                "erase_held": self.brush.erase_held,
                "armed": self.brush.armed,
                "locked": self.brush.locked,
                "limit_to_colour": self.brush.limit,
                "limit_reason": self.brush.limit_reason,
            }),
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

/// One mode's position in the host's own list of modes, which is the list the Add row offers and the
/// controller resolves a chosen index against.
pub(crate) fn mode_index(mode: ComponentMode) -> usize {
    rules::MODES
        .iter()
        .position(|known| *known == mode)
        .unwrap_or(0)
}

/// A host sentence as a line the panel shows on its own: its first letter capitalised, nothing else
/// changed.
fn sentence(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// A refusal's own words, without the kind the API prefixes them with.
fn reason(refused: Result<(), luxforge_core::Error>) -> Option<String> {
    refused.err().map(|error| error.detail)
}

/// Why a new mask cannot start in `mode`: a mask's first component is always an add, so rather than
/// creating an add while the Add row says subtract — which would be a silent coercion — New mask is
/// refused and says why, in the host's words for the rule.
pub(crate) fn create_mode_reason(mode: ComponentMode) -> Option<String> {
    (!rules::may_lead(mode)).then(|| {
        format!(
            "{}; the next component is set to {}",
            sentence(rules::FIRST_COMPONENT_IS_ADD),
            mode.as_str()
        )
    })
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
        create_reason: reason(rules::room_for_mask(reports.len()))
            .or_else(|| create_mode_reason(inputs.mask_mode)),
        add_reason: open.and_then(|report| {
            reason(rules::room_for_component(
                &report.name,
                report.components.len(),
            ))
        }),
        masks,
        selected,
        controls: mask_controls(inputs, enabled && open.is_some()),
        components,
        kinds: kinds(enabled),
        modes: rules::MODES
            .iter()
            .map(|mode| mode.as_str().to_owned())
            .collect(),
        add_mode: mode_index(inputs.mask_mode),
        disabled_reason,
        enabled,
        draft: draft_model(inputs, enabled),
        brush: brush_model(inputs, enabled, open),
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
        .map(|component| rules::unknown_kind(&component.kind).detail)
}

/// Every kind the Add row and New mask can actually create, as they offer it. That is the kinds
/// whose geometry is declared as numbers, not every kind the build can evaluate: a drawn kind like
/// the brush is parsed, evaluated and retained but generates no `mask.create-<kind>`, so offering it
/// here would put up a button with no command behind it. The list is still the host's, so
/// registering a kind with declared geometry is what puts it here.
///
/// Such a kind reaches the panel one of two ways, and the host's own declarations decide which. A
/// kind this build draws handles for starts a **gesture**; a kind whose geometry is entirely
/// defaulted is **typed** and is created straight away, as the selection its defaults describe, then
/// narrowed through the number fields its own declarations generate. A kind that is neither says so
/// on its button rather than offering an action that would do nothing.
fn kinds(enabled: bool) -> Vec<KindOption> {
    luxforge_core::mask::declared_geometry_kinds()
        .map(|kind| KindOption {
            kind: kind.to_owned(),
            label: luxforge_core::mask::kind_title(kind),
            drawable: crate::mask_draft::drawable(kind),
            typed: luxforge_core::mask::component_geometry_is_defaulted(kind),
            enabled,
        })
        .collect()
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
    luxforge_core::mask::commands::controls()
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
fn control_action(control: &luxforge_core::Control) -> Option<&str> {
    match classify(control) {
        Rendered::Number { action, .. }
        | Rendered::Toggle { action, .. }
        | Rendered::Choice { action, .. }
        | Rendered::Color { action, .. }
        | Rendered::Curve { action, .. }
        | Rendered::Action { action, .. }
        | Rendered::Presets { action } => Some(action),
        // A group, a picker and a module worker task submit no mask command, so none is offered here.
        Rendered::Group { .. }
        | Rendered::Picker { .. }
        | Rendered::Task { .. }
        | Rendered::Unsupported(_) => None,
    }
}

/// The open mask's component list, with each row's refusals resolved from the command family's own
/// rules rather than discovered by sending a request that will be rejected.
fn component_rows(report: &MaskReport, inputs: &Inputs<'_>, enabled: bool) -> Vec<ComponentRow> {
    let modes = declared_modes();
    let component_modes: Vec<ComponentMode> = report
        .components
        .iter()
        .map(|component| component.mode)
        .collect();
    report
        .components
        .iter()
        .map(|component| {
            let first = component.index == 0;
            let selected = inputs.selected_component == Some(&component.id);
            // A mask's first component is always `add`: nothing precedes it to subtract from or
            // intersect with, so the mode control is not offered rather than offered and refused,
            // and the rule is stated in the host's words.
            let mode_reason = first.then(|| sentence(rules::FIRST_COMPONENT_IS_ADD));
            // A mask never exists empty, so its last component is removed by removing the mask.
            let delete_reason = reason(rules::delete_component(
                &report.name,
                report.components.len(),
            ));
            ComponentRow {
                // Moving a row into or out of the leading position is refused whenever it would
                // leave a component that is not an add at the front, which is the same rule the
                // host checks; the panel states it here instead of offering the move.
                up_reason: move_reason(report, &component_modes, component.index, -1, enabled),
                down_reason: move_reason(report, &component_modes, component.index, 1, enabled),
                // The first component's mode is fixed by the composition, so its control is not
                // offered; every other row carries its own, showing that component's mode.
                mode_options: if first { Vec::new() } else { modes.clone() },
                mode_selected: modes
                    .iter()
                    .position(|option| option == component.mode.as_str())
                    .unwrap_or(0),
                mode_label: control_label("mask.set-component-mode", "mode"),
                invert_label: control_label("mask.set-component-invert", "invert"),
                fields: if selected {
                    kind_fields(inputs, &component.kind, enabled && component.available)
                } else {
                    Vec::new()
                },
                id: component.id.clone(),
                index: component.index,
                name: component.name.clone(),
                kind_title: luxforge_core::mask::kind_title(&component.kind),
                kind: component.kind.clone(),
                mode: component.mode.as_str().to_owned(),
                inverted: component.invert,
                available: component.available,
                selected,
                hovered: inputs.hovered_component == Some(&component.id),
                mode_reason,
                delete_reason,
                can_edit_shape: enabled
                    && component.available
                    && crate::mask_draft::drawable(&component.kind),
                painted: crate::mask_draft::paintable(&component.kind),
                strokes: if selected {
                    stroke_rows(&component.payload, &component.name, enabled)
                } else {
                    Vec::new()
                },
                samples: if selected {
                    sample_rows(&component.payload, &component.kind, enabled)
                } else {
                    Vec::new()
                },
                can_pick: luxforge_core::mask::component_sample_limit(&component.kind).is_some(),
                picking: pick_mode(&component.kind)
                    .is_some_and(|mode| inputs.session.workspace.mode == mode),
                pick_label: pick_label(&component.kind),
                pick_reason: pick_reason(report, component, enabled),
                limits: if selected {
                    kind_limits(&component.kind)
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// What one kind does not select, read from the host's own kind table rather than from a list here.
///
/// A position-based kind answers nothing: a gradient and a brush select where they were drawn, and no
/// value in the picture changes that. A kind registered later carries its own statement without this
/// being touched, which is the whole reason the sentences live in the table.
fn kind_limits(kind: &str) -> Vec<String> {
    luxforge_core::mask::component_kind_limits(kind)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The canvas mode one kind's pick lives in, which is that pick's own action, or none when the kind
/// samples nothing. The panel reads the host's declaration and names no mode of its own.
pub(crate) fn pick_mode(kind: &str) -> Option<String> {
    kind_pick(kind).map(|(action, _)| action.to_owned())
}

/// What the pick button reads, from the host's own declared title.
fn pick_label(kind: &str) -> String {
    kind_pick(kind)
        .map(|(_, title)| title.to_owned())
        .unwrap_or_else(|| "Pick".to_owned())
}

/// The host's own pick for one kind, found by the sample method the host generated for it: the pick's
/// mode is that method's name, and its title is what the button reads.
fn kind_pick(kind: &str) -> Option<(&'static str, &'static str)> {
    let method = commands::sample(SampleOp::Add, kind)?.method;
    match commands::canvas_pick(method)? {
        luxforge_core::CanvasInteraction::SampleApply { title, .. } => Some((method, title)),
        _ => None,
    }
}

/// Why a colour cannot be picked into this component right now, in the words the command family
/// would use. Every one of these is a state the host itself refuses, stated before the click rather
/// than discovered by sending a request that will be rejected.
fn pick_reason(report: &MaskReport, component: &ComponentReport, enabled: bool) -> Option<String> {
    if !enabled {
        return Some("Waiting for the last request".into());
    }
    if !component.available {
        return Some(rules::unknown_kind(&component.kind).detail);
    }
    // A pick reads the pixel the operation this mask modulates receives, so there has to be an
    // operation: the host refuses a mask no layer is bound to, and the panel says so first.
    if let Some(unbound) = reason(rules::bound_layer(&report.name, !report.layers.is_empty())) {
        return Some(unbound);
    }
    let limit = luxforge_core::mask::component_sample_limit(&component.kind)?;
    let held = component.payload[luxforge_core::mask::SAMPLES_FIELD]
        .as_array()
        .map_or(0, Vec::len);
    reason(rules::room_for_sample(
        &component.name,
        &component.kind,
        held,
        limit,
    ))
}

/// The colours one component's stored payload holds, read through the host's own reserved field so
/// the panel parses no payload of its own.
fn sample_rows(payload: &serde_json::Value, kind: &str, enabled: bool) -> Vec<SampleRow> {
    if luxforge_core::mask::component_sample_limit(kind).is_none() {
        return Vec::new();
    }
    let held = payload[luxforge_core::mask::SAMPLES_FIELD]
        .as_array()
        .cloned()
        .unwrap_or_default();
    held.iter()
        .enumerate()
        .filter_map(|(index, sample)| {
            let channels: Vec<f64> = sample
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_f64)
                .collect();
            let [r, g, b] = <[f64; 3]>::try_from(channels).ok()?;
            Some(SampleRow {
                index,
                label: format!("Colour {}", index + 1),
                text: format!("{r:.3}, {g:.3}, {b:.3}"),
                swatch: [code(r), code(g), code(b)],
                delete_reason: (!enabled).then(|| "Waiting for the last request".to_owned()),
            })
        })
        .collect()
}

/// One linear-sRGB channel as the 8-bit code a swatch draws, through the delivered encode so the chip
/// shows the colour the value means rather than a guess at it.
fn code(linear: f64) -> u8 {
    let encoded = luxforge_core::colour::srgb::encode(linear.max(0.0));
    (255.0 * encoded.clamp(0.0, 1.0) + 0.5).floor() as u8
}

/// The strokes one component's stored payload references, read through the host's own reserved
/// field so the panel parses no payload of its own. A payload that carries none — every gradient's —
/// gives no rows, and a malformed one gives none rather than a guess.
fn stroke_rows(payload: &serde_json::Value, component: &str, enabled: bool) -> Vec<StrokeRow> {
    let held = luxforge_core::path::references(payload, component).unwrap_or_default();
    held.iter()
        .enumerate()
        .map(|(index, stroke)| StrokeRow {
            stroke: stroke.as_str().to_owned(),
            index,
            label: format!("Stroke {}", index + 1),
            delete_reason: if !enabled {
                Some("Waiting for the last request".into())
            } else {
                reason(rules::delete_stroke(stroke.as_str(), component, held.len()))
            },
        })
        .collect()
}

/// The brush the next stroke will be drawn with, as the panel offers it.
///
/// Every field is generated from `mask.add-stroke`'s own declarations — its name, its range, its
/// step and its precision — so the panel names no setting of its own and a key and a nudge move by
/// the same declared amount. **There is no density**: its Lightroom meaning needs a build-up model
/// along one stroke, which would make coverage depend on stamp spacing and therefore on resolution,
/// so it is left out and the [user guide](../../../docs/user-guide.md) says why rather than the
/// panel implying it exists.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrushModel {
    pub(crate) fields: Vec<DraftField>,
    pub(crate) erase: bool,
    pub(crate) erase_label: String,
    /// The next stroke is held to the colour under the brush where it begins. It is **not** Auto
    /// Mask: a per-pixel colour test with no notion of an edge, which the label and the guide say.
    pub(crate) limit: bool,
    pub(crate) limit_label: String,
    /// Why the limit cannot apply to the next stroke, when it cannot: it reads the pixel the masked
    /// operation receives, so the open mask has to be bound to a layer.
    pub(crate) limit_reason: Option<String>,
    /// The erase modifier is held down, so the next stroke erases whatever the toggle says.
    pub(crate) erase_held: bool,
    /// A stroke is on the photograph, so the brush is frozen for the rest of its life.
    pub(crate) locked: bool,
    /// A painted gesture is open and waiting for the pointer.
    pub(crate) armed: bool,
    /// Painting on the open mask is possible: one is open.
    pub(crate) can_add: bool,
    pub(crate) enabled: bool,
}

/// The brush settings and what they can be put down on.
fn brush_model(inputs: &Inputs<'_>, enabled: bool, open: Option<&MaskReport>) -> BrushModel {
    let brush = inputs.brush;
    let painting = inputs
        .mask_draft
        .and_then(MaskDraft::brush)
        .is_some_and(|stroke| stroke.painting());
    let declared = luxforge_core::mask::commands::find(ADD_STROKE);
    let fields = brush
        .values()
        .into_iter()
        .map(|(name, value)| {
            let spec = declared
                .and_then(|command| command.action.parameter(name))
                .and_then(NumberSpec::of);
            DraftField {
                label: luxforge_core::mask::kind_title(name),
                text: spec.map_or_else(|| format!("{value:.4}"), |spec| spec.format(value)),
                step: spec.map_or(0.01, |spec| spec.step),
                name: name.to_owned(),
                value,
            }
        })
        .collect();
    // The limit reads the pixel the operation the open mask modulates receives, so it needs an
    // operation: the host refuses a mask no layer is bound to by name, and the panel states that
    // before the stroke rather than after it. `limit` is what the next stroke will actually carry,
    // which is why it is the toggle's state *and* the condition, in one place.
    let limit_reason = limit_reason(open);
    BrushModel {
        fields,
        erase: brush.erase,
        erase_label: luxforge_core::mask::kind_title("erase"),
        limit: brush.limit_to_colour && limit_reason.is_none(),
        limit_label: luxforge_core::mask::kind_title("limit_to_colour"),
        limit_reason,
        erase_held: inputs.brush_erase_held,
        locked: painting,
        armed: inputs
            .mask_draft
            .is_some_and(|draft| draft.brush().is_some()),
        can_add: open.is_some(),
        enabled,
    }
}

/// Why the next stroke cannot be limited to a colour, or `None` when it can.
///
/// One predicate, read by the panel and by the gesture that builds the request, so what the panel
/// says and what the stroke carries cannot disagree.
pub(crate) fn limit_reason(open: Option<&MaskReport>) -> Option<String> {
    let Some(report) = open else {
        // With no mask open the next stroke draws a new one, which no layer is bound to yet.
        return Some(rules::limit_on_new_mask().detail);
    };
    reason(rules::bound_layer(&report.name, !report.layers.is_empty()))
}

/// The one host command every stroke commits through.
const ADD_STROKE: &str = luxforge_core::mask::commands::ADD_STROKE;

/// The mode tokens the host's own `mask.set-component-mode` declares, in declared order. The panel
/// offers exactly these and invents none: a mode a control shows is a mode the command accepts.
fn declared_modes() -> Vec<String> {
    luxforge_core::mask::commands::find("mask.set-component-mode")
        .and_then(|command| command.action.parameter("mode"))
        .and_then(|declared| match &declared.kind {
            luxforge_core::ParameterKind::Enum { options } => Some(options.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The label the host's own control declares for one command's parameter.
fn control_label(action: &str, parameter: &str) -> String {
    crate::state::tools::labelled_control(
        luxforge_core::mask::commands::controls(),
        action,
        parameter,
    )
    .unwrap_or(parameter)
    .to_owned()
}

/// Why one component cannot move by `step` places, or `None` when it can: the host's own refusal of
/// that reorder, for the modes the listing reports.
fn move_reason(
    report: &MaskReport,
    modes: &[ComponentMode],
    index: usize,
    step: i64,
    enabled: bool,
) -> Option<String> {
    if !enabled {
        return Some("Waiting for the last request".into());
    }
    let Ok(target) = u64::try_from(index as i64 + step) else {
        return Some(format!("{} is already at the top of the list", report.name));
    };
    reason(rules::reorder_component(&report.name, modes, index, target))
}

/// One kind's declared geometry fields, which are the host controls whose action is that kind's own
/// patch method. The panel names no field: registering a kind brings its fields with it.
fn kind_fields(inputs: &Inputs<'_>, kind: &str, enabled: bool) -> Vec<ControlModel> {
    let Some(command) = luxforge_core::mask::commands::geometry(
        luxforge_core::mask::commands::GeometryOp::Set,
        kind,
    ) else {
        return Vec::new();
    };
    host_controls(inputs, enabled, |action| action == command.method)
}

fn draft_model(inputs: &Inputs<'_>, enabled: bool) -> Option<MaskDraftModel> {
    let draft: &MaskDraft = inputs.mask_draft?;
    let apply_reason = if inputs.gesture_conflicted {
        Some("Changed elsewhere: discard the draft or reapply it".into())
    } else if !enabled {
        disabled_reason(inputs)
    } else if draft.method().is_none() {
        Some(format!(
            "This build cannot draw a {} component",
            draft.kind()
        ))
    } else {
        None
    };
    // The fields the open gesture offers, from the same declarations its commit is validated
    // against: the panel names no field and no step of its own. A painted gesture's fields are its
    // command's, because a brush has no patch method to read them from.
    let patch = if draft.brush().is_some() {
        luxforge_core::mask::commands::find(ADD_STROKE)
    } else {
        luxforge_core::mask::commands::geometry(
            luxforge_core::mask::commands::GeometryOp::Set,
            draft.kind(),
        )
    };
    let fields = draft
        .values()
        .into_iter()
        .map(|(name, value)| {
            let spec = patch
                .and_then(|command| command.action.parameter(name))
                .and_then(NumberSpec::of);
            DraftField {
                name: name.to_owned(),
                label: crate::state::tools::labelled_control(
                    luxforge_core::mask::commands::controls(),
                    patch.map(|command| command.method).unwrap_or_default(),
                    name,
                )
                .map(str::to_owned)
                .unwrap_or_else(|| luxforge_core::mask::kind_title(name)),
                text: spec.map_or_else(|| format!("{value:.4}"), |spec| spec.format(value)),
                value,
                step: spec.map_or(0.01, |spec| spec.step),
            }
        })
        .collect();
    Some(MaskDraftModel {
        fields,
        title: draft.op.label().to_owned(),
        method: draft.method().unwrap_or_default().to_owned(),
        kind: draft.kind().to_owned(),
        readout: draft
            .values()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), format!("{value:.4}")))
            .collect(),
        conflicted: inputs.gesture_conflicted,
        can_apply: apply_reason.is_none(),
        apply_reason,
        painted: draft.brush().is_some(),
    })
}
