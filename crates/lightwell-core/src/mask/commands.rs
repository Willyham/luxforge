//! The `mask.*` host command family: what each command declares, what it does to a recipe's mask
//! table, and the history label it commits.
//!
//! Masks are host commands in their own namespace, as `history.*` and `version.*` are, and not a
//! tool module (`docs/design/masking.md#host-commands`). The reason is structural: a module commits
//! *layers* through [`crate::ActionPlan`] and must never rewrite the recipe, while every one of these
//! commands rewrites the mask table beside the layers. What they share with a module is everything
//! else, and deliberately the *same* code rather than a parallel copy of it:
//!
//! - Each command declares an [`ActionDescriptor`] with [`ParameterDescriptor`]s of the same closed
//!   [`crate::ParameterKind`] set a module may declare, so [`check_parameters`] is the one and only
//!   path a value takes to be validated, whatever client sent it, and a refusal reads the same.
//! - [`controls`] declares the panel widgets over those same parameters with the same [`Control`]
//!   type, so there is one control-generation path and a client renders a mask's number fields,
//!   toggles and mode selector with the widgets it already has.
//! - `schema.list` lists every command from this table through [`MaskCommand::schema`], so discovery
//!   and dispatch cannot drift apart.
//! - A command commits through the delivered [`crate::EditorService::commit_snapshot`] path, so the
//!   mutation envelope, the input-hash deduplication, the revision check, the single history entry
//!   and the single immutable snapshot are the delivered ones and not a second implementation.
//! - A gesture drafts through the delivered `draft.begin` / `draft.set` / `draft.commit` lifecycle
//!   with its existing conflict, Discard and Reapply behaviour; see [`MaskTarget`] for the one thing
//!   a draft needed that a declared parameter cannot carry.
//!
//! What no declared parameter kind can express is an identity or a person's free text: the kinds are
//! numbers, integers, enums, colours, booleans and curves. So the mask a command addresses, the
//! component inside it and the name a rename sets travel in the request's envelope beside
//! `asset_id` and `mutation`, as [`MaskTarget`], and only geometry, amounts, modes and flags are
//! declared parameters. That is why the masking design needs no string parameter kind.
use super::{POSITION_MAX, POSITION_MIN, knows_component_kind};
use crate::{
    ActionDescriptor, ChoiceStyle, Component, ComponentId, ComponentMode, Control, Error,
    ErrorKind, Layer, LayerId, Mask, MaskId, ModuleRegistry, MutationResult, NumberStyle,
    ParameterDescriptor, ParameterKind, Recipe,
    model::{COMPONENTS_PER_MASK, MASKS_PER_RECIPE},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, sync::LazyLock};

/// The one read-only method of the family.
pub const LIST: &str = "mask.list";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// A component kind this build cannot evaluate, in the spelling
/// [`super::validate_component_kinds`] already uses, because it is the same fact: a payload the host
/// retains and cannot read. `Incompatible` and not `Validation` — the stored stack is well formed and
/// this build simply cannot draw part of it.
fn unknown_kind(kind: &str) -> Error {
    Error::new(
        ErrorKind::Incompatible,
        format!("unknown mask component {kind}"),
    )
}

/// The host-owned envelope of one mask command: the objects it addresses and the display name a
/// rename sets.
///
/// These are not declared parameters and cannot be. The closed parameter-kind set carries numbers,
/// integers, enums, colours, booleans and curves — no identity and no free text — so a mask
/// identity travels beside `asset_id` exactly as `asset_id` itself does, and a client that can read
/// `mask.list` can address anything in it. It is also what a drafted gesture carries in
/// [`crate::Draft::target`]: the handle drag edits *that* component, and which component it is
/// cannot live in the drafted fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One declared `mask.*` method.
#[derive(Debug)]
pub struct MaskCommand {
    /// The method name, which is also the durable action identity a history entry stores. It carries
    /// a dot, which [`crate::valid_name`] forbids inside an action identity, so a module action and a
    /// mask command cannot collide however either grows; [`crate::ModuleRegistry::register`] checks
    /// that rather than assuming it.
    pub method: &'static str,
    pub mutates: bool,
    pub needs_mask: bool,
    pub needs_component: bool,
    pub needs_name: bool,
    pub action: ActionDescriptor,
}

impl MaskCommand {
    /// The envelope fields this command requires and refuses, checked before anything is planned so
    /// a malformed request never reaches a recipe. The wording matches the delivered envelope
    /// refusal, because it is the same kind of missing field.
    pub fn checked_target(&self, target: &MaskTarget) -> Result<(), Error> {
        for (present, needed, field) in [
            (target.mask.is_some(), self.needs_mask, "mask"),
            (
                target.component.is_some(),
                self.needs_component,
                "component",
            ),
            (target.name.is_some(), self.needs_name, "name"),
        ] {
            if needed && !present {
                return Err(validation(format!(
                    "missing required field {field} for {}",
                    self.method
                )));
            }
            if present && !needed {
                return Err(validation(format!(
                    "unknown field {field} for {}",
                    self.method
                )));
            }
        }
        Ok(())
    }

    /// This command's entry in `schema.list`, in the shape a generated action method uses: the
    /// envelope first, then the command's own declared parameters, then the descriptors themselves so
    /// a client generates its controls from the same declaration the host validates against.
    pub fn schema(&self) -> Value {
        let mut required = vec![json!("asset_id")];
        let mut optional = Map::new();
        if self.mutates {
            required.push(json!("mutation"));
        } else {
            optional.insert(
                "entry_id".to_owned(),
                json!("entry to ask about; default the session's selection"),
            );
        }
        for (needed, field, meaning) in [
            (self.needs_mask, "mask", "the mask this command addresses"),
            (
                self.needs_component,
                "component",
                "the component inside that mask",
            ),
            (self.needs_name, "name", "the display name to set"),
        ] {
            if needed {
                required.push(json!(field));
                optional.remove(field);
                let _ = meaning;
            }
        }
        for parameter in &self.action.parameters {
            if !self.action.patch && parameter.required && parameter.default.is_none() {
                required.push(json!(parameter.name));
            } else {
                optional.insert(parameter.name.clone(), json!(parameter.notes));
            }
        }
        json!({
            "mutates": self.mutates,
            "patch": self.action.patch,
            "required": required,
            "optional": optional,
            "notes": self.action.notes,
            "parameters": self.action.parameters,
        })
    }
}

/// Every declared command, in the order the design's method table lists them.
pub fn all() -> &'static [MaskCommand] {
    &COMMANDS
}

/// The command one method name declares, or none. The same lookup discovery, dispatch, drafting and
/// the registry's collision check all use, so the four cannot drift.
pub fn find(method: &str) -> Option<&'static MaskCommand> {
    COMMANDS.iter().find(|command| command.method == method)
}

/// The panel widgets of the mask commands, over the parameters those commands declare.
///
/// The same [`Control`] vocabulary a module declares, so a client generates a gradient's endpoint
/// fields, the whole-mask amount, the three-way mode selector and the two inversion toggles with the
/// widgets it already has, and no client invents an operation of its own. Layout — which row a
/// control sits in — belongs to the Masks panel and not here.
pub fn controls() -> &'static [Control] {
    &CONTROLS
}

/// What one command does to a stack.
#[derive(Debug)]
pub(crate) enum MaskOutcome {
    /// The command changed nothing: a drag returned to its start, a value was set to what it already
    /// was. No entry is written, exactly as a module's [`crate::ActionPlan::NoOp`].
    NoOp,
    Change(MaskChange),
}

/// The resulting stack, the label the entry stores, and what the command says it touched.
#[derive(Debug)]
pub(crate) struct MaskChange {
    pub recipe: Recipe,
    pub label: String,
    pub mask: Option<MaskId>,
    pub component: Option<ComponentId>,
    pub removed_layers: Vec<RemovedLayer>,
}

/// One layer a destructive command removed, named by the module that provided its effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovedLayer {
    pub id: LayerId,
    pub effect: String,
    /// The provider's title, such as `Basic`, or none when no provider declares the effect. It is
    /// what the history label names, so a destructive delete reads as a person would say it.
    pub title: Option<String>,
}

/// What one mask command answers with: the delivered mutation result, flattened so `outcome`,
/// `revision` and `deduplicated` read exactly where every other mutation puts them, plus what this
/// command changed.
///
/// A no-op reports the envelope alone. Nothing durable was written, so there is nothing to read back
/// on a retry, and a retried request must answer identically to the call it retries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskCommandResult {
    #[serde(flatten)]
    pub mutation: MutationResult,
    /// The history label this command committed, which is the durable record of what it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentId>,
    /// The layers a `mask.delete` removed. A destructive command says what it removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_layers: Vec<RemovedLayer>,
}

impl MaskCommandResult {
    /// A result carrying the envelope and nothing else: a no-op, and a retry of one.
    pub(crate) fn plain(mutation: MutationResult) -> Self {
        Self {
            mutation,
            label: None,
            mask: None,
            component: None,
            removed_layers: Vec::new(),
        }
    }
}

/// The report of a command read back from the entry it wrote, so a deduplicated retry answers
/// identically to the call it retries rather than approximately.
///
/// Everything is recovered from durable data. The label is what the entry stores; a mask or component
/// the request named is in the entry's parameters; and one the *command* minted — a created mask, a
/// duplicate, an appended component — is recovered by comparing the entry's stack with its parent's,
/// which is the same comparison that recovers the layers a delete removed. A minted identity cannot
/// be in the request's parameters: they are the deduplication identity, and a fresh identity there
/// would make every attempt hash differently and never deduplicate at all.
pub(crate) fn report_of(
    mutation: MutationResult,
    entry: &crate::HistoryEntry,
    parent: Option<&crate::HistoryEntry>,
    registry: &ModuleRegistry,
) -> MaskCommandResult {
    let named = |field: &str| {
        entry
            .parameters
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let before = parent.map(|parent| &parent.snapshot.recipe);
    let after = &entry.snapshot.recipe;
    let (mask, component) = match entry.action_id.as_str() {
        // A new mask and the one component it was created with.
        "mask.create" => {
            let added = added_mask(after, before);
            (
                added.map(|mask| mask.id.clone()),
                added
                    .and_then(|mask| mask.components.first())
                    .map(|component| component.id.clone()),
            )
        }
        // The copy, not the source the request named.
        "mask.duplicate" => (added_mask(after, before).map(|mask| mask.id.clone()), None),
        // The mask is the one the request named; the component is the one it appended.
        "mask.add-component" => {
            let mask = named("mask").and_then(|id| MaskId::parse(id).ok());
            let component = mask
                .as_ref()
                .and_then(|id| added_component(after, before, id))
                .map(|component| component.id.clone());
            (mask, component)
        }
        _ => (
            named("mask").and_then(|id| MaskId::parse(id).ok()),
            named("component").and_then(|id| ComponentId::parse(id).ok()),
        ),
    };
    let removed = parent.map_or_else(Vec::new, |parent| {
        parent
            .snapshot
            .recipe
            .layers
            .iter()
            .filter(|layer| {
                !entry
                    .snapshot
                    .recipe
                    .layers
                    .iter()
                    .any(|kept| kept.id == layer.id)
            })
            .map(|layer| removed_layer(layer, registry))
            .collect()
    });
    MaskCommandResult {
        mutation,
        label: Some(entry.label.clone()),
        mask,
        component,
        removed_layers: removed,
    }
}

/// The mask an entry's stack carries and its parent's did not.
fn added_mask<'a>(after: &'a Recipe, before: Option<&Recipe>) -> Option<&'a Mask> {
    after.masks.iter().find(|mask| match before {
        Some(before) => !before.masks.iter().any(|had| had.id == mask.id),
        None => true,
    })
}

/// The component one mask gained between an entry's parent and the entry.
fn added_component<'a>(
    after: &'a Recipe,
    before: Option<&Recipe>,
    mask: &MaskId,
) -> Option<&'a Component> {
    let had = before
        .and_then(|before| before.masks.iter().find(|candidate| &candidate.id == mask))
        .map(|mask| &mask.components);
    after
        .masks
        .iter()
        .find(|candidate| &candidate.id == mask)?
        .components
        .iter()
        .find(|component| match had {
            Some(had) => !had.iter().any(|kept| kept.id == component.id),
            None => true,
        })
}

/// `mask.list`: every mask of one stack with its components, its values and the layers bound to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskListing {
    pub entry_id: crate::EntryId,
    pub masks: Vec<MaskReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskReport {
    pub id: MaskId,
    /// Position in the mask list, which is the order masked layers of one effect are evaluated in.
    pub index: usize,
    pub name: String,
    pub amount: f64,
    pub invert: bool,
    pub components: Vec<ComponentReport>,
    pub layers: Vec<MaskedLayer>,
}

impl Eq for MaskReport {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentReport {
    pub id: ComponentId,
    pub index: usize,
    pub name: String,
    pub mode: ComponentMode,
    pub invert: bool,
    pub kind: String,
    pub payload: Value,
    /// Whether this build can evaluate the kind. A stored kind it cannot is reported here and kept
    /// byte for byte, exactly as a layer whose effect has no provider is reported and kept.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskedLayer {
    pub id: LayerId,
    pub effect: String,
    pub module: Option<String>,
    pub title: Option<String>,
}

/// The listing of one stored stack. Read-only in every sense: it reads the snapshot it was handed
/// and touches nothing.
pub(crate) fn listing(
    entry_id: crate::EntryId,
    recipe: &Recipe,
    registry: &ModuleRegistry,
) -> MaskListing {
    let masks = recipe
        .masks
        .iter()
        .enumerate()
        .map(|(index, mask)| MaskReport {
            id: mask.id.clone(),
            index,
            name: mask.name.clone(),
            amount: mask.amount,
            invert: mask.invert,
            components: mask
                .components
                .iter()
                .enumerate()
                .map(|(index, component)| ComponentReport {
                    id: component.id.clone(),
                    index,
                    name: component.name.clone(),
                    mode: component.mode,
                    invert: component.invert,
                    kind: component.kind.clone(),
                    payload: component.payload.clone(),
                    available: knows_component_kind(&component.kind),
                })
                .collect(),
            layers: recipe
                .layers
                .iter()
                .filter(|layer| layer.mask.as_ref() == Some(&mask.id))
                .map(|layer| {
                    let descriptor = registry
                        .effect(&layer.effect_id)
                        .map(|(module, _)| module.descriptor());
                    MaskedLayer {
                        id: layer.id.clone(),
                        effect: layer.effect_id.clone(),
                        module: descriptor.map(|descriptor| descriptor.id.clone()),
                        title: descriptor.map(|descriptor| descriptor.title.clone()),
                    }
                })
                .collect(),
        })
        .collect();
    MaskListing { entry_id, masks }
}

/// The parameters one command stores on its history entry, which are also its deduplication
/// identity: its declared parameters plus the envelope fields that say *which* objects it addressed.
///
/// The envelope belongs in the identity: `mask.set-invert {invert: true}` on two different masks are
/// two different requests, and a shared request id with different input must be a `conflict` rather
/// than a silently reused result.
pub(crate) fn stored_parameters(
    checked: &Map<String, Value>,
    target: &MaskTarget,
) -> Map<String, Value> {
    let mut stored = checked.clone();
    if let Some(mask) = &target.mask {
        stored.insert("mask".to_owned(), json!(mask.as_str()));
    }
    if let Some(component) = &target.component {
        stored.insert("component".to_owned(), json!(component.as_str()));
    }
    if let Some(name) = &target.name {
        stored.insert("name".to_owned(), json!(name));
    }
    stored
}

/// What one command would do to this stack: the whole family's behaviour in one place.
///
/// Pure — it reads the recipe it is handed and returns a new one — so the same function answers a
/// commit and a drafted preview, and a drafted gesture therefore previews exactly the stack
/// committing it would write. It reads no pixels and costs `O(masks + components + layers)`.
pub(crate) fn plan(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    registry: &ModuleRegistry,
) -> Result<MaskOutcome, Error> {
    command.checked_target(target)?;
    let mut next = recipe.clone();
    // Each arm produces the base label, whether that label already names the mask, and what the
    // command touched. The mask prefix is applied once, below, so there is one label rule.
    let (base, names_mask, mask_id, component_id, removed) = match command.method {
        "mask.create" => {
            if next.masks.len() >= MASKS_PER_RECIPE {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks per recipe"
                    ),
                ));
            }
            let kind = enumeration(parameters, "kind")?;
            let mut mask = Mask::new(next_mask_name(&next));
            // The ordinal comes from the mask and is spent there, so it is never reused.
            let name = mask.next_component_name(kind);
            // The first component of a mask is always `add`: there is nothing yet to subtract from
            // or intersect with, so this command declares no mode at all and a request that sends
            // one is refused by the generic parameter check.
            let component = Component::new(
                name,
                ComponentMode::Add,
                kind,
                geometry_payload(kind, parameters)?,
            );
            let component_id = component.id.clone();
            let mask_id = mask.id.clone();
            mask.components.push(component);
            mask.validate()?;
            next.masks.push(mask);
            (
                format!("Add {}", spoken(kind)),
                false,
                Some(mask_id),
                Some(component_id),
                Vec::new(),
            )
        }
        "mask.delete" => {
            let index = mask_index(&next, required_mask(target)?)?;
            let mask = next.masks.remove(index);
            // Deleting a mask deletes the layers bound to it. It is destructive, so the label and
            // the result both name what went with it.
            let removed: Vec<RemovedLayer> = next
                .layers
                .iter()
                .filter(|layer| layer.mask.as_ref() == Some(&mask.id))
                .map(|layer| removed_layer(layer, registry))
                .collect();
            next.layers
                .retain(|layer| layer.mask.as_ref() != Some(&mask.id));
            let base = match spoken_titles(&removed) {
                Some(titles) => format!("Delete {} with {titles}", mask.name),
                None => format!("Delete {}", mask.name),
            };
            (base, true, Some(mask.id), None, removed)
        }
        "mask.rename" => {
            let index = mask_index(&next, required_mask(target)?)?;
            let name = target
                .name
                .clone()
                .ok_or_else(|| validation("missing required field name for mask.rename"))?;
            let previous = std::mem::replace(&mut next.masks[index].name, name.clone());
            next.masks[index].validate()?;
            let id = next.masks[index].id.clone();
            (
                format!("Rename {previous} to {name}"),
                true,
                Some(id),
                None,
                Vec::new(),
            )
        }
        "mask.duplicate" => {
            if next.masks.len() >= MASKS_PER_RECIPE {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks per recipe"
                    ),
                ));
            }
            let index = mask_index(&next, required_mask(target)?)?;
            let source = next.masks[index].clone();
            let mut copy = source.clone();
            copy.id = MaskId::new();
            copy.name = next_mask_name(&next);
            // New identities, the same geometry and the same spent ordinals: the copy's next linear
            // component is `Linear 2`, because `Linear 1` already names one of its components.
            for component in &mut copy.components {
                component.id = ComponentId::new();
            }
            copy.validate()?;
            let id = copy.id.clone();
            next.masks.insert(index + 1, copy);
            (
                format!("Duplicate {}", source.name),
                true,
                Some(id),
                None,
                Vec::new(),
            )
        }
        "mask.set-amount" => {
            let index = mask_index(&next, required_mask(target)?)?;
            let amount = number(parameters, "amount")?;
            next.masks[index].amount = amount;
            next.masks[index].validate()?;
            let id = next.masks[index].id.clone();
            (
                format!("Amount {amount}"),
                false,
                Some(id),
                None,
                Vec::new(),
            )
        }
        "mask.set-invert" => {
            let index = mask_index(&next, required_mask(target)?)?;
            let invert = boolean(parameters, "invert")?;
            next.masks[index].invert = invert;
            let id = next.masks[index].id.clone();
            (
                inversion_label(invert).to_owned(),
                false,
                Some(id),
                None,
                Vec::new(),
            )
        }
        "mask.reorder" => {
            let index = mask_index(&next, required_mask(target)?)?;
            let to = position(parameters, "index", next.masks.len(), "masks")?;
            let mask = next.masks.remove(index);
            let (id, name) = (mask.id.clone(), mask.name.clone());
            next.masks.insert(to, mask);
            // Masked layers of one effect are evaluated in their masks' order, so moving a mask
            // moves them with it, in this one transaction, and nothing else moves.
            resort_masked_layers(&mut next.layers, &next.masks);
            (
                format!("Move {name} to {}", to + 1),
                true,
                Some(id),
                None,
                Vec::new(),
            )
        }
        "mask.add-component" => {
            let index = mask_index(&next, required_mask(target)?)?;
            let mask = &mut next.masks[index];
            if mask.components.len() >= COMPONENTS_PER_MASK {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "mask {} has {COMPONENTS_PER_MASK} components; the limit is {COMPONENTS_PER_MASK} components per mask",
                        mask.name
                    ),
                ));
            }
            let kind = enumeration(parameters, "kind")?;
            let mode = mode(parameters)?;
            let name = mask.next_component_name(kind);
            let component = Component::new(name, mode, kind, geometry_payload(kind, parameters)?);
            let component_id = component.id.clone();
            mask.components.push(component);
            mask.validate()?;
            let base = if mode == ComponentMode::Add {
                format!("Add {}", spoken(kind))
            } else {
                format!("Add {} {}", mode.as_str(), spoken(kind))
            };
            let id = mask.id.clone();
            (base, false, Some(id), Some(component_id), Vec::new())
        }
        "mask.set-component" => {
            let (mask_index, index) = component_at(&next, target)?;
            let mask = &mut next.masks[mask_index];
            let component = &mut mask.components[index];
            let fields =
                geometry_fields(&component.kind).ok_or_else(|| unknown_kind(&component.kind))?;
            let mut payload = component
                .payload
                .as_object()
                .cloned()
                .ok_or_else(|| unknown_kind(&component.kind))?;
            for (field, value) in parameters {
                if !fields.contains(&field.as_str()) {
                    return Err(validation(format!(
                        "parameter {field} is not part of a {} component",
                        component.kind
                    )));
                }
                payload.insert(field.clone(), canonical(value));
            }
            component.payload = Value::Object(payload);
            // A later edit to a component names that component, so the row says which one it was.
            let base = format!("Update {}", component.name);
            let component_id = component.id.clone();
            mask.validate()?;
            let id = mask.id.clone();
            (base, false, Some(id), Some(component_id), Vec::new())
        }
        "mask.set-component-mode" => {
            let (mask_index, index) = component_at(&next, target)?;
            let mode = mode(parameters)?;
            let mask = &mut next.masks[mask_index];
            mask.components[index].mode = mode;
            let base = format!("{} {}", mask.components[index].name, mode.as_str());
            let component_id = mask.components[index].id.clone();
            // The first component of a mask is always add, so promoting one to subtract or
            // intersect is refused here with the model's own reason.
            mask.validate()?;
            let id = mask.id.clone();
            (base, false, Some(id), Some(component_id), Vec::new())
        }
        "mask.set-component-invert" => {
            let (mask_index, index) = component_at(&next, target)?;
            let invert = boolean(parameters, "invert")?;
            let mask = &mut next.masks[mask_index];
            mask.components[index].invert = invert;
            let base = format!(
                "{} {}",
                mask.components[index].name,
                inversion_label(invert).to_lowercase()
            );
            let component_id = mask.components[index].id.clone();
            let id = mask.id.clone();
            (base, false, Some(id), Some(component_id), Vec::new())
        }
        "mask.delete-component" => {
            let (mask_index, index) = component_at(&next, target)?;
            let mask = &mut next.masks[mask_index];
            // A mask never exists empty from a command, so its last component is not deletable:
            // deleting the mask is the command that removes it, and it says what it removed.
            if mask.components.len() == 1 {
                return Err(validation(format!(
                    "mask {} has one component; delete the mask rather than its last component",
                    mask.name
                )));
            }
            let removed = mask.components.remove(index);
            // Removing the leading add component of a mask whose next component subtracts leaves a
            // mask that cannot be read; it is refused with the model's reason rather than promoted.
            mask.validate()?;
            let id = mask.id.clone();
            (
                format!("Delete {}", removed.name),
                false,
                Some(id),
                Some(removed.id),
                Vec::new(),
            )
        }
        "mask.reorder-component" => {
            let (mask_index, index) = component_at(&next, target)?;
            let mask = &mut next.masks[mask_index];
            let to = position(parameters, "index", mask.components.len(), "components")?;
            let component = mask.components.remove(index);
            let (component_id, name) = (component.id.clone(), component.name.clone());
            mask.components.insert(to, component);
            mask.validate()?;
            let id = mask.id.clone();
            (
                format!("Move {name}"),
                false,
                Some(id),
                Some(component_id),
                Vec::new(),
            )
        }
        other => {
            return Err(validation(format!("{other} changes no mask")));
        }
    };
    // Nothing changed: the value was already that, or a drag returned to where it began. No entry
    // and no event, exactly as a module's no-op.
    if next == *recipe {
        return Ok(MaskOutcome::NoOp);
    }
    let label = mask_label(base, names_mask, mask_id.as_ref(), &next);
    Ok(MaskOutcome::Change(MaskChange {
        recipe: next,
        label,
        mask: mask_id,
        component: component_id,
        removed_layers: removed,
    }))
}

/// The one label rule: the command's own text, prefixed with the mask's name whenever the resulting
/// stack carries more than one mask, because a history list shared with every other module cannot
/// afford `Update Linear 1` alone.
///
/// It is evaluated on the *resulting* mask table, so the mask a `mask.create` just made is counted;
/// and a command whose text already names its mask — rename, duplicate, reorder and the destructive
/// delete — is not prefixed with it twice.
fn mask_label(base: String, names_mask: bool, mask: Option<&MaskId>, resulting: &Recipe) -> String {
    if names_mask || resulting.masks.len() < 2 {
        return base;
    }
    match mask.and_then(|id| resulting.masks.iter().find(|mask| &mask.id == id)) {
        Some(mask) => format!("{} · {base}", mask.name),
        None => base,
    }
}

/// `Inverted` and `Not inverted`, one spelling for both levels of inversion.
fn inversion_label(invert: bool) -> &'static str {
    if invert { "Inverted" } else { "Not inverted" }
}

/// A kind as a label says it: `linear`, `luminance range`. Lower case, because the design's history
/// rows read `Add brush` and `Add subtract brush`.
fn spoken(kind: &str) -> String {
    kind.replace('-', " ")
}

/// The provider titles of the layers a destructive command removed, in stack order and without
/// repeating a title: `Basic, Presence`.
fn spoken_titles(removed: &[RemovedLayer]) -> Option<String> {
    let mut titles: Vec<&str> = Vec::new();
    for layer in removed {
        let title = layer.title.as_deref().unwrap_or(layer.effect.as_str());
        if !titles.contains(&title) {
            titles.push(title);
        }
    }
    if titles.is_empty() {
        None
    } else {
        Some(titles.join(", "))
    }
}

fn removed_layer(layer: &Layer, registry: &ModuleRegistry) -> RemovedLayer {
    RemovedLayer {
        id: layer.id.clone(),
        effect: layer.effect_id.clone(),
        title: registry
            .effect(&layer.effect_id)
            .map(|(module, _)| module.descriptor().title.clone()),
    }
}

/// The lowest unused default mask name, so `Mask 1` freed by a delete is available again while a
/// name a person typed is never taken.
fn next_mask_name(recipe: &Recipe) -> String {
    (1..=MASKS_PER_RECIPE + 1)
        .map(|ordinal| format!("Mask {ordinal}"))
        .find(|candidate| !recipe.masks.iter().any(|mask| &mask.name == candidate))
        .unwrap_or_else(|| format!("Mask {}", recipe.masks.len() + 1))
}

/// Masked layers of one effect are evaluated in their masks' order, so a mask move re-sorts exactly
/// those layers and nothing else: every position in the stack that held a masked layer of an effect
/// still holds one, and the layers at those positions are permuted into the new mask order. A global
/// layer, an unmasked layer and every other effect keep their position exactly.
fn resort_masked_layers(layers: &mut [Layer], masks: &[Mask]) {
    let rank = |id: &MaskId| {
        masks
            .iter()
            .position(|mask| &mask.id == id)
            .unwrap_or(usize::MAX)
    };
    let mut positions: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, layer) in layers.iter().enumerate() {
        if layer.mask.is_some() {
            positions
                .entry(layer.effect_id.clone())
                .or_default()
                .push(index);
        }
    }
    for indices in positions.values() {
        let mut taken: Vec<Layer> = indices.iter().map(|&index| layers[index].clone()).collect();
        // Stable, so two layers of one effect on one mask keep their stored order.
        taken.sort_by_key(|layer| layer.mask.as_ref().map_or(usize::MAX, rank));
        for (&index, layer) in indices.iter().zip(taken) {
            layers[index] = layer;
        }
    }
}

fn required_mask(target: &MaskTarget) -> Result<&MaskId, Error> {
    target
        .mask
        .as_ref()
        .ok_or_else(|| validation("missing required field mask"))
}

fn mask_index(recipe: &Recipe, id: &MaskId) -> Result<usize, Error> {
    recipe
        .masks
        .iter()
        .position(|mask| &mask.id == id)
        .ok_or_else(|| validation(format!("unknown mask {id}")))
}

/// The mask and the component one component command addresses.
fn component_at(recipe: &Recipe, target: &MaskTarget) -> Result<(usize, usize), Error> {
    let mask = mask_index(recipe, required_mask(target)?)?;
    let id = target
        .component
        .as_ref()
        .ok_or_else(|| validation("missing required field component"))?;
    let index = recipe.masks[mask]
        .components
        .iter()
        .position(|component| &component.id == id)
        .ok_or_else(|| {
            validation(format!(
                "mask {} has no component {id}",
                recipe.masks[mask].name
            ))
        })?;
    Ok((mask, index))
}

/// The declared payload fields of one component kind, whose names are the declared parameter names
/// a command builds that payload from, so a geometry parameter and a stored field are one spelling.
fn geometry_fields(kind: &str) -> Option<&'static [&'static str]> {
    GEOMETRY
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map(|(_, fields)| *fields)
}

/// The stored payload of a new component of `kind`, built from that kind's declared geometry.
///
/// Numbers are canonicalized to `f64`, so a client that sends `0` where another sends `0.0` writes
/// the same bytes, and setting a field back to what it already held is recognized as a no-op.
fn geometry_payload(kind: &str, parameters: &Map<String, Value>) -> Result<Value, Error> {
    let fields = geometry_fields(kind).ok_or_else(|| unknown_kind(kind))?;
    let mut payload = Map::new();
    for field in fields {
        let value = parameters.get(*field).ok_or_else(|| {
            validation(format!(
                "missing required parameter {field} for a {kind} component"
            ))
        })?;
        payload.insert((*field).to_owned(), canonical(value));
    }
    // A geometry parameter another kind declares must not reach this one.
    for name in parameters.keys() {
        if name != "kind" && name != "mode" && !fields.contains(&name.as_str()) {
            return Err(validation(format!(
                "parameter {name} is not part of a {kind} component"
            )));
        }
    }
    Ok(Value::Object(payload))
}

/// Every number a mask command stores is an `f64`, whatever JSON spelling arrived. The generic check
/// already refused anything that is not a finite number.
fn canonical(value: &Value) -> Value {
    match value.as_f64() {
        Some(number) => json!(number),
        None => value.clone(),
    }
}

fn enumeration<'a>(parameters: &'a Map<String, Value>, name: &str) -> Result<&'a str, Error> {
    parameters
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))
}

fn number(parameters: &Map<String, Value>, name: &str) -> Result<f64, Error> {
    parameters
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))
}

fn boolean(parameters: &Map<String, Value>, name: &str) -> Result<bool, Error> {
    parameters
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))
}

fn mode(parameters: &Map<String, Value>) -> Result<ComponentMode, Error> {
    match enumeration(parameters, "mode")? {
        "add" => Ok(ComponentMode::Add),
        "subtract" => Ok(ComponentMode::Subtract),
        "intersect" => Ok(ComponentMode::Intersect),
        other => Err(validation(format!("unknown component mode {other}"))),
    }
}

/// A destination index inside a list that currently holds `len` items. The declared range bounds the
/// request; this bounds it against the list it actually addresses, and names what it counted.
fn position(
    parameters: &Map<String, Value>,
    name: &str,
    len: usize,
    what: &str,
) -> Result<usize, Error> {
    let index = parameters
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| validation(format!("missing required parameter {name}")))?;
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    if index >= len {
        return Err(validation(format!(
            "index {index} is outside the {len} {what} of this stack"
        )));
    }
    Ok(index)
}

/// Every component kind's declared geometry. A later kind is one more row with its own payload
/// fields, and the commands pick up its parameters from the same place.
const GEOMETRY: &[(&str, &[&str])] = &[("linear", &["x0", "y0", "x1", "y1"])];

/// The kinds a command may create. One entry today; the table above and this list are the same
/// table read two ways.
fn kinds() -> Vec<String> {
    GEOMETRY
        .iter()
        .map(|(kind, _)| (*kind).to_owned())
        .collect()
}

fn modes() -> Vec<String> {
    ["add", "subtract", "intersect"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn parameter(name: &str, kind: ParameterKind, required: bool, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.to_owned(),
        kind,
        required,
        default: None,
        unit: None,
        step: None,
        precision: None,
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
        notes: notes.to_owned(),
    }
}

/// One normalized stored position, the parameter kind every gradient endpoint takes: a fraction of
/// the content stage with one stage extent of overshoot on each side.
fn position_parameter(name: &str, required: bool) -> ParameterDescriptor {
    ParameterDescriptor {
        step: Some(0.01),
        precision: Some(4),
        soft_min: Some(0.0),
        soft_max: Some(1.0),
        fine_step: Some(0.001),
        ..parameter(
            name,
            ParameterKind::Number {
                min: POSITION_MIN,
                max: POSITION_MAX,
            },
            required,
            "normalized content-stage position; the frame is 0..1 and one stage extent of overshoot is legal on each side",
        )
    }
}

/// The geometry parameters a command that creates or patches a component declares. With one
/// component kind this is that kind's fields; a second kind adds its own, and the per-kind check in
/// [`geometry_payload`] is what keeps one kind's field off another kind's component.
fn geometry_parameters(required: bool) -> Vec<ParameterDescriptor> {
    GEOMETRY
        .iter()
        .flat_map(|(_, fields)| fields.iter())
        .map(|field| position_parameter(field, required))
        .collect()
}

fn command(
    method: &'static str,
    title: &str,
    notes: &str,
    mutates: bool,
    needs: (bool, bool, bool),
    patch: bool,
    parameters: Vec<ParameterDescriptor>,
) -> MaskCommand {
    MaskCommand {
        method,
        mutates,
        needs_mask: needs.0,
        needs_component: needs.1,
        needs_name: needs.2,
        action: ActionDescriptor {
            id: method.to_owned(),
            title: title.to_owned(),
            notes: notes.to_owned(),
            // A mask label names the objects it touched and the mask it belongs to, which no
            // template over declared parameters can render; `plan` renders it at commit and the
            // entry stores it, exactly as a module's rendered `summary` is stored.
            summary: None,
            patch,
            parameters,
        },
    }
}

static COMMANDS: LazyLock<Vec<MaskCommand>> = LazyLock::new(|| {
    let kind = || {
        parameter(
            "kind",
            ParameterKind::Enum { options: kinds() },
            true,
            "the component kind to create",
        )
    };
    let mode = |required| {
        parameter(
            "mode",
            ParameterKind::Enum { options: modes() },
            required,
            "how this component joins the coverage the components before it composed",
        )
    };
    let invert = |notes: &str| parameter("invert", ParameterKind::Boolean, true, notes);
    let index = |limit: i64, notes: &str| {
        parameter(
            "index",
            ParameterKind::Integer {
                min: 0,
                max: limit - 1,
            },
            true,
            notes,
        )
    };
    let mut create = vec![kind()];
    create.extend(geometry_parameters(true));
    let mut add = vec![kind(), mode(true)];
    add.extend(geometry_parameters(true));
    vec![
        command(
            LIST,
            "Masks",
            "every mask of one stack with its components, values, amount, invert and the layers bound to it; read-only, writes no history and emits no event",
            false,
            (false, false, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.create",
            "New mask",
            "a new mask whose first component is an add component of the named kind; a mask never exists empty, so the initial component is required and its mode is always add",
            true,
            (false, false, false),
            false,
            create,
        ),
        command(
            "mask.delete",
            "Delete mask",
            "delete a mask and the layers bound to it; destructive, so the history label and the result both name the layers it removed",
            true,
            (true, false, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.rename",
            "Rename mask",
            "set a mask's display name; a name is a person's text and never an identity",
            true,
            (true, false, true),
            false,
            Vec::new(),
        ),
        command(
            "mask.duplicate",
            "Duplicate mask",
            "a copy of a mask and its components, with new identities, placed after it; the layers bound to the original stay with the original",
            true,
            (true, false, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.set-amount",
            "Amount",
            "the whole-mask amount multiplying the composed coverage",
            true,
            (true, false, false),
            false,
            vec![ParameterDescriptor {
                step: Some(1.0),
                precision: Some(0),
                ..parameter(
                    "amount",
                    ParameterKind::Number {
                        min: 0.0,
                        max: Mask::FULL_AMOUNT,
                    },
                    true,
                    "0..=100, multiplying the composed coverage",
                )
            }],
        ),
        command(
            "mask.set-invert",
            "Invert mask",
            "invert the composed coverage of a whole mask, before its amount",
            true,
            (true, false, false),
            false,
            vec![invert("invert the composed coverage before the amount")],
        ),
        command(
            "mask.reorder",
            "Move mask",
            "move a mask in the masks list and, with it, the masked layers of every effect, in one transaction; nothing else moves",
            true,
            (true, false, false),
            false,
            vec![index(
                MASKS_PER_RECIPE as i64,
                "the mask's new position in the masks list",
            )],
        ),
        command(
            "mask.add-component",
            "Add component",
            "a second, third, … component of a mask, with its mode given explicitly rather than guessed from a modifier key",
            true,
            (true, false, false),
            false,
            add,
        ),
        command(
            "mask.set-component",
            "Update component",
            "a field patch over one component's geometry; the fields the request names are validated and merged over the stored payload",
            true,
            (true, true, false),
            true,
            geometry_parameters(false),
        ),
        command(
            "mask.set-component-mode",
            "Component mode",
            "change a component's role in the composition after the fact; the first component of a mask is always add",
            true,
            (true, true, false),
            false,
            vec![mode(true)],
        ),
        command(
            "mask.set-component-invert",
            "Invert component",
            "invert one component's own coverage before it is combined",
            true,
            (true, true, false),
            false,
            vec![invert(
                "invert this component's coverage before it is combined",
            )],
        ),
        command(
            "mask.delete-component",
            "Delete component",
            "remove one component from a mask; a mask is never empty, so its last component is not deletable",
            true,
            (true, true, false),
            false,
            Vec::new(),
        ),
        command(
            "mask.reorder-component",
            "Move component",
            "move a component inside its mask; the composition reads the list in order",
            true,
            (true, true, false),
            false,
            vec![index(
                COMPONENTS_PER_MASK as i64,
                "the component's new position in its mask's component list",
            )],
        ),
    ]
});

static CONTROLS: LazyLock<Vec<Control>> = LazyLock::new(|| {
    let mut controls = vec![
        Control::Number {
            action: "mask.set-amount".to_owned(),
            parameter: "amount".to_owned(),
            label: "Amount".to_owned(),
            style: NumberStyle::Slider,
            rail: None,
        },
        Control::Toggle {
            action: "mask.set-invert".to_owned(),
            parameter: "invert".to_owned(),
            label: "Invert".to_owned(),
        },
        Control::Choice {
            action: "mask.set-component-mode".to_owned(),
            parameter: "mode".to_owned(),
            label: "Mode".to_owned(),
            style: ChoiceStyle::Segmented,
        },
        Control::Toggle {
            action: "mask.set-component-invert".to_owned(),
            parameter: "invert".to_owned(),
            label: "Invert component".to_owned(),
        },
    ];
    // Both endpoints of a gradient have number fields, and every later kind's geometry gets its
    // fields the same way, from the parameters the patch command declares.
    controls.extend(
        GEOMETRY
            .iter()
            .flat_map(|(_, fields)| fields.iter())
            .map(|field| Control::Number {
                action: "mask.set-component".to_owned(),
                parameter: (*field).to_owned(),
                label: field.to_uppercase(),
                style: NumberStyle::Field,
                rail: None,
            }),
    );
    controls
});

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ModuleRegistry {
        ModuleRegistry::builtin()
    }

    fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Map<String, Value> {
        let mut parameters = Map::new();
        parameters.insert("kind".into(), json!("linear"));
        for (name, value) in [("x0", x0), ("y0", y0), ("x1", x1), ("y1", y1)] {
            parameters.insert(name.into(), json!(value));
        }
        parameters
    }

    fn apply(
        recipe: &Recipe,
        method: &str,
        target: MaskTarget,
        parameters: Map<String, Value>,
    ) -> Result<MaskChange, Error> {
        let command = find(method).expect("a declared command");
        match plan(command, recipe, &target, &parameters, &registry())? {
            MaskOutcome::NoOp => panic!("{method} changed nothing"),
            MaskOutcome::Change(change) => Ok(change),
        }
    }

    fn created() -> (Recipe, MaskChange) {
        let recipe = Recipe::default();
        let change = apply(
            &recipe,
            "mask.create",
            MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
        )
        .unwrap();
        (change.recipe.clone(), change)
    }

    #[test]
    fn the_declared_family_is_the_designs_method_table() {
        let methods: Vec<&str> = all().iter().map(|command| command.method).collect();
        assert_eq!(
            methods,
            [
                "mask.list",
                "mask.create",
                "mask.delete",
                "mask.rename",
                "mask.duplicate",
                "mask.set-amount",
                "mask.set-invert",
                "mask.reorder",
                "mask.add-component",
                "mask.set-component",
                "mask.set-component-mode",
                "mask.set-component-invert",
                "mask.delete-component",
                "mask.reorder-component",
            ]
        );
        assert!(
            all()
                .iter()
                .all(|command| command.method == command.action.id),
            "a command's method name is its durable action identity"
        );
        assert!(
            all()
                .iter()
                .all(|command| !crate::valid_name(command.method)),
            "a mask command identity can never be a module action identity"
        );
        // Only `mask.list` reads; every other command is an ordinary mutation.
        let reading: Vec<&str> = all()
            .iter()
            .filter(|command| !command.mutates)
            .map(|command| command.method)
            .collect();
        assert_eq!(reading, [LIST]);
    }

    #[test]
    fn every_control_binds_to_a_parameter_its_own_command_declares() {
        for control in controls() {
            let (action, parameter) = match control {
                Control::Number {
                    action, parameter, ..
                }
                | Control::Toggle {
                    action, parameter, ..
                }
                | Control::Choice {
                    action, parameter, ..
                } => (action, parameter),
                other => panic!("unexpected mask control {other:?}"),
            };
            let command = find(action).unwrap_or_else(|| panic!("no command {action}"));
            assert!(
                command.action.parameter(parameter).is_some(),
                "{action} declares no parameter {parameter}"
            );
        }
    }

    #[test]
    fn a_created_mask_carries_one_add_component_named_from_its_kind() {
        let (recipe, change) = created();
        assert_eq!(recipe.masks.len(), 1);
        let mask = &recipe.masks[0];
        assert_eq!(mask.name, "Mask 1");
        assert_eq!(mask.amount, Mask::FULL_AMOUNT);
        assert!(!mask.invert);
        assert_eq!(mask.components.len(), 1);
        assert_eq!(mask.components[0].name, "Linear 1");
        assert_eq!(mask.components[0].mode, ComponentMode::Add);
        assert_eq!(
            mask.components[0].payload,
            json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":1.0})
        );
        assert_eq!(change.label, "Add linear", "one mask needs no prefix");
        assert_eq!(change.mask.as_ref(), Some(&mask.id));
        assert_eq!(change.component.as_ref(), Some(&mask.components[0].id));
    }

    #[test]
    fn an_ordinal_is_never_reused_so_one_label_means_one_component() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let target = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        let mut add = linear(0.2, 0.0, 0.8, 1.0);
        add.insert("mode".into(), json!("subtract"));
        let second = apply(&recipe, "mask.add-component", target.clone(), add).unwrap();
        assert_eq!(second.recipe.masks[0].components[1].name, "Linear 2");
        assert_eq!(second.label, "Add subtract linear");
        // Delete the second and add another of the same kind: the freed ordinal is not reused.
        let component = second.recipe.masks[0].components[1].id.clone();
        let deleted = apply(
            &second.recipe,
            "mask.delete-component",
            MaskTarget {
                component: Some(component),
                ..target.clone()
            },
            Map::new(),
        )
        .unwrap();
        assert_eq!(deleted.label, "Delete Linear 2");
        assert_eq!(deleted.recipe.masks[0].components.len(), 1);
        let mut again = linear(0.3, 0.0, 0.9, 1.0);
        again.insert("mode".into(), json!("intersect"));
        let third = apply(&deleted.recipe, "mask.add-component", target, again).unwrap();
        assert_eq!(
            third.recipe.masks[0].components[1].name, "Linear 3",
            "an entry reading Update Linear 2 can only ever mean the component it was written about"
        );
    }

    #[test]
    fn a_later_edit_names_the_component_and_a_second_mask_names_the_mask() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let component = recipe.masks[0].components[0].id.clone();
        let target = MaskTarget {
            mask: Some(mask.clone()),
            component: Some(component.clone()),
            name: None,
        };
        let mut patch = Map::new();
        patch.insert("y1".into(), json!(0.6));
        let updated = apply(&recipe, "mask.set-component", target.clone(), patch.clone()).unwrap();
        assert_eq!(updated.label, "Update Linear 1");
        assert_eq!(
            updated.recipe.masks[0].components[0].payload,
            json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":0.6}),
            "a patch merges over the stored payload"
        );
        // A second mask exists, so every row that does not already name its mask names it.
        let two = apply(
            &updated.recipe,
            "mask.create",
            MaskTarget::default(),
            linear(1.0, 0.0, 1.0, 1.0),
        )
        .unwrap();
        assert_eq!(two.label, "Mask 2 · Add linear");
        let again = apply(&two.recipe, "mask.set-component", target, {
            let mut patch = Map::new();
            patch.insert("y1".into(), json!(0.4));
            patch
        })
        .unwrap();
        assert_eq!(again.label, "Mask 1 · Update Linear 1");
    }

    #[test]
    fn every_row_of_the_designs_granularity_table_reads_as_it_states() {
        let (one, create) = created();
        assert_eq!(create.label, "Add linear");
        let mask = one.masks[0].id.clone();
        let component = one.masks[0].components[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        let of_component = MaskTarget {
            component: Some(component),
            ..of_mask.clone()
        };
        // "Drag a radial's handle" → `Update Radial 1`: the same rule over the kind that exists.
        let dragged = apply(&one, "mask.set-component", of_component.clone(), {
            let mut patch = Map::new();
            patch.insert("x1".into(), json!(0.5));
            patch
        })
        .unwrap();
        assert_eq!(dragged.label, "Update Linear 1");
        // "Add a subtract brush to the same mask" → `Add subtract brush`.
        let mut add = linear(0.1, 0.1, 0.9, 0.9);
        add.insert("mode".into(), json!("subtract"));
        let added = apply(&dragged.recipe, "mask.add-component", of_mask.clone(), add).unwrap();
        assert_eq!(added.label, "Add subtract linear");
        // "Change Brush 2 to intersect" → `Brush 2 intersect`.
        let second = added.recipe.masks[0].components[1].id.clone();
        let mut mode = Map::new();
        mode.insert("mode".into(), json!("intersect"));
        let changed = apply(
            &added.recipe,
            "mask.set-component-mode",
            MaskTarget {
                component: Some(second),
                ..of_mask.clone()
            },
            mode,
        )
        .unwrap();
        assert_eq!(changed.label, "Linear 2 intersect");
        // The whole-mask modifiers and the component inversion take the same shape.
        let mut amount = Map::new();
        amount.insert("amount".into(), json!(60.0));
        assert_eq!(
            apply(&one, "mask.set-amount", of_mask.clone(), amount)
                .unwrap()
                .label,
            "Amount 60"
        );
        let mut invert = Map::new();
        invert.insert("invert".into(), json!(true));
        assert_eq!(
            apply(&one, "mask.set-invert", of_mask, invert.clone())
                .unwrap()
                .label,
            "Inverted"
        );
        assert_eq!(
            apply(&one, "mask.set-component-invert", of_component, invert)
                .unwrap()
                .label,
            "Linear 1 inverted"
        );
    }

    #[test]
    fn a_change_that_changes_nothing_writes_no_entry() {
        let (recipe, _) = created();
        let target = MaskTarget {
            mask: Some(recipe.masks[0].id.clone()),
            component: Some(recipe.masks[0].components[0].id.clone()),
            name: None,
        };
        let command = find("mask.set-component").unwrap();
        // The drag ended where it began: the stored payload, in either JSON spelling of a number.
        for value in [json!(1.0), json!(1)] {
            let mut patch = Map::new();
            patch.insert("y1".into(), value);
            assert!(
                matches!(
                    plan(command, &recipe, &target, &patch, &registry()).unwrap(),
                    MaskOutcome::NoOp
                ),
                "returning to the start is a no-op"
            );
        }
        for (method, name, value) in [
            ("mask.set-amount", "amount", json!(100.0)),
            ("mask.set-invert", "invert", json!(false)),
        ] {
            let mut parameters = Map::new();
            parameters.insert(name.into(), value);
            let command = find(method).unwrap();
            let target = MaskTarget {
                mask: Some(recipe.masks[0].id.clone()),
                ..MaskTarget::default()
            };
            assert!(matches!(
                plan(command, &recipe, &target, &parameters, &registry()).unwrap(),
                MaskOutcome::NoOp
            ));
        }
    }

    #[test]
    fn a_mask_never_exists_empty_and_never_begins_by_subtracting() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let component = recipe.masks[0].components[0].id.clone();
        let target = MaskTarget {
            mask: Some(mask),
            component: Some(component),
            name: None,
        };
        let only = find("mask.delete-component").unwrap();
        assert_eq!(
            plan(only, &recipe, &target, &Map::new(), &registry())
                .unwrap_err()
                .detail,
            "mask Mask 1 has one component; delete the mask rather than its last component"
        );
        let mut mode = Map::new();
        mode.insert("mode".into(), json!("subtract"));
        assert_eq!(
            plan(
                find("mask.set-component-mode").unwrap(),
                &recipe,
                &target,
                &mode,
                &registry()
            )
            .unwrap_err()
            .detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
    }

    #[test]
    fn a_reorder_that_puts_a_subtract_first_is_refused_with_the_models_reason() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask),
            ..MaskTarget::default()
        };
        let mut add = linear(0.1, 0.1, 0.9, 0.9);
        add.insert("mode".into(), json!("subtract"));
        let two = apply(&recipe, "mask.add-component", of_mask.clone(), add).unwrap();
        let second = two.recipe.masks[0].components[1].id.clone();
        let mut index = Map::new();
        index.insert("index".into(), json!(0));
        let error = plan(
            find("mask.reorder-component").unwrap(),
            &two.recipe,
            &MaskTarget {
                component: Some(second.clone()),
                ..of_mask.clone()
            },
            &index,
            &registry(),
        )
        .unwrap_err();
        assert_eq!(
            error.detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
        // Deleting the leading add would leave the same unreadable mask, so it is refused too.
        let first = two.recipe.masks[0].components[0].id.clone();
        assert_eq!(
            plan(
                find("mask.delete-component").unwrap(),
                &two.recipe,
                &MaskTarget {
                    component: Some(first),
                    ..of_mask.clone()
                },
                &Map::new(),
                &registry()
            )
            .unwrap_err()
            .detail,
            "mask Mask 1 begins with a subtract component; the first component of a mask is always \
             add"
        );
        // Moving it to the position it already holds changes nothing at all.
        let mut index = Map::new();
        index.insert("index".into(), json!(1));
        assert!(
            matches!(
                plan(
                    find("mask.reorder-component").unwrap(),
                    &two.recipe,
                    &MaskTarget {
                        component: Some(second),
                        ..of_mask
                    },
                    &index,
                    &registry()
                )
                .unwrap(),
                MaskOutcome::NoOp
            ),
            "moving a component to the position it holds is a no-op"
        );
    }

    #[test]
    fn refusals_name_what_they_refuse() {
        let (recipe, _) = created();
        let stranger = Mask::new("Mask 9");
        assert_eq!(
            plan(
                find("mask.set-invert").unwrap(),
                &recipe,
                &MaskTarget {
                    mask: Some(stranger.id.clone()),
                    ..MaskTarget::default()
                },
                &{
                    let mut fields = Map::new();
                    fields.insert("invert".into(), json!(true));
                    fields
                },
                &registry()
            )
            .unwrap_err()
            .detail,
            format!("unknown mask {}", stranger.id)
        );
        let absent = ComponentId::new();
        assert_eq!(
            plan(
                find("mask.set-component").unwrap(),
                &recipe,
                &MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    component: Some(absent.clone()),
                    name: None,
                },
                &Map::new(),
                &registry()
            )
            .unwrap_err()
            .detail,
            format!("mask Mask 1 has no component {absent}")
        );
        // The envelope is checked before anything is planned.
        assert_eq!(
            find("mask.delete")
                .unwrap()
                .checked_target(&MaskTarget::default())
                .unwrap_err()
                .detail,
            "missing required field mask for mask.delete"
        );
        assert_eq!(
            find("mask.create")
                .unwrap()
                .checked_target(&MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    ..MaskTarget::default()
                })
                .unwrap_err()
                .detail,
            "unknown field mask for mask.create"
        );
    }

    #[test]
    fn a_rename_names_both_names_and_a_duplicate_takes_new_identities() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let of_mask = MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        };
        let renamed = apply(
            &recipe,
            "mask.rename",
            MaskTarget {
                name: Some("Sky".into()),
                ..of_mask.clone()
            },
            Map::new(),
        )
        .unwrap();
        assert_eq!(renamed.label, "Rename Mask 1 to Sky");
        assert_eq!(renamed.recipe.masks[0].name, "Sky");
        // A rename to the name a mask already has changes nothing.
        assert!(matches!(
            plan(
                find("mask.rename").unwrap(),
                &renamed.recipe,
                &MaskTarget {
                    name: Some("Sky".into()),
                    ..of_mask.clone()
                },
                &Map::new(),
                &registry()
            )
            .unwrap(),
            MaskOutcome::NoOp
        ));
        let copied = apply(&renamed.recipe, "mask.duplicate", of_mask, Map::new()).unwrap();
        assert_eq!(copied.label, "Duplicate Sky");
        assert_eq!(copied.recipe.masks.len(), 2);
        let (source, copy) = (&copied.recipe.masks[0], &copied.recipe.masks[1]);
        assert_ne!(source.id, copy.id);
        assert_ne!(source.components[0].id, copy.components[0].id);
        assert_eq!(copy.name, "Mask 1", "the lowest unused default name");
        assert_eq!(copy.components[0].payload, source.components[0].payload);
        assert_eq!(
            copy.components[0].name, "Linear 1",
            "component names are unique within a mask, not across masks"
        );
        assert_eq!(
            copy.next_ordinal, source.next_ordinal,
            "the copy's next linear is Linear 2, because Linear 1 already names one of its own"
        );
    }

    #[test]
    fn a_deleted_mask_takes_its_layers_and_says_which() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let mut recipe = recipe;
        for effect in [crate::BASIC_EFFECT, crate::PRESENCE_EFFECT] {
            recipe.layers.push(Layer {
                id: LayerId::new(),
                effect_id: effect.to_owned(),
                effect_format: crate::EFFECT_FORMAT,
                payload: json!({}),
                mask: Some(mask.clone()),
            });
        }
        let global = Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
        };
        recipe.layers.insert(0, global.clone());
        let deleted = apply(
            &recipe,
            "mask.delete",
            MaskTarget {
                mask: Some(mask),
                ..MaskTarget::default()
            },
            Map::new(),
        )
        .unwrap();
        assert_eq!(deleted.label, "Delete Mask 1 with Basic, Presence");
        assert_eq!(deleted.removed_layers.len(), 2);
        assert_eq!(
            deleted
                .removed_layers
                .iter()
                .map(|layer| layer.title.clone().unwrap())
                .collect::<Vec<_>>(),
            ["Basic", "Presence"]
        );
        assert_eq!(
            deleted.recipe.layers,
            vec![global],
            "the global layer of the same effect stays exactly where it was"
        );
        assert!(deleted.recipe.masks.is_empty());
    }

    #[test]
    fn a_mask_move_resorts_the_masked_layers_and_moves_nothing_else() {
        let (recipe, _) = created();
        let first = recipe.masks[0].id.clone();
        let two = apply(
            &recipe,
            "mask.create",
            MaskTarget::default(),
            linear(1.0, 0.0, 1.0, 1.0),
        )
        .unwrap();
        let second = two.recipe.masks[1].id.clone();
        let mut recipe = two.recipe.clone();
        let masked = |effect: &str, mask: &MaskId| Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: Some(mask.clone()),
        };
        let global = Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
        };
        recipe.layers = vec![
            global.clone(),
            masked(crate::BASIC_EFFECT, &first),
            masked(crate::BASIC_EFFECT, &second),
            masked(crate::PRESENCE_EFFECT, &first),
            masked(crate::PRESENCE_EFFECT, &second),
        ];
        let before = recipe.layers.clone();
        let mut index = Map::new();
        index.insert("index".into(), json!(0));
        let moved = apply(
            &recipe,
            "mask.reorder",
            MaskTarget {
                mask: Some(second.clone()),
                ..MaskTarget::default()
            },
            index,
        )
        .unwrap();
        assert_eq!(moved.label, "Move Mask 2 to 1");
        assert_eq!(
            moved
                .recipe
                .masks
                .iter()
                .map(|mask| mask.name.as_str())
                .collect::<Vec<_>>(),
            ["Mask 2", "Mask 1"]
        );
        assert_eq!(
            moved.recipe.layers[0], global,
            "an unmasked layer never moves"
        );
        assert_eq!(
            moved.recipe.layers[1], before[2],
            "the masked Basic layers swap, in their masks' new order"
        );
        assert_eq!(moved.recipe.layers[2], before[1]);
        assert_eq!(
            moved.recipe.layers[3], before[4],
            "and so do the masked Presence layers, independently"
        );
        assert_eq!(moved.recipe.layers[4], before[3]);
    }

    #[test]
    fn the_listing_reports_values_components_and_the_bound_layers() {
        let (recipe, _) = created();
        let mask = recipe.masks[0].id.clone();
        let mut recipe = recipe;
        let layer = Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: Some(mask.clone()),
        };
        recipe.layers.push(layer.clone());
        // A kind this build cannot evaluate is listed, kept and reported as unavailable.
        let mut future = Mask::new("Future");
        let name = future.next_component_name("cloud");
        future
            .components
            .push(Component::new(name, ComponentMode::Add, "cloud", json!({})));
        recipe.masks.push(future);
        let entry = crate::EntryId::new();
        let listed = listing(entry.clone(), &recipe, &registry());
        assert_eq!(listed.entry_id, entry);
        assert_eq!(listed.masks.len(), 2);
        let report = &listed.masks[0];
        assert_eq!(report.index, 0);
        assert_eq!(report.name, "Mask 1");
        assert_eq!(report.amount, Mask::FULL_AMOUNT);
        assert!(!report.invert);
        assert_eq!(report.components.len(), 1);
        assert_eq!(report.components[0].name, "Linear 1");
        assert_eq!(report.components[0].kind, "linear");
        assert!(report.components[0].available);
        assert_eq!(
            report.components[0].payload,
            json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":1.0})
        );
        assert_eq!(report.layers.len(), 1);
        assert_eq!(report.layers[0].id, layer.id);
        assert_eq!(report.layers[0].title.as_deref(), Some("Basic"));
        assert_eq!(listed.masks[1].components[0].name, "Cloud 1");
        assert!(
            !listed.masks[1].components[0].available,
            "a kind this build does not know is named, kept and reported"
        );
        assert!(listed.masks[1].layers.is_empty());
    }

    #[test]
    fn a_geometry_edit_on_a_kind_this_build_cannot_read_is_incompatible() {
        let mut recipe = Recipe::default();
        let mut future = Mask::new("Mask 1");
        let name = future.next_component_name("cloud");
        future
            .components
            .push(Component::new(name, ComponentMode::Add, "cloud", json!({})));
        let (mask, component) = (future.id.clone(), future.components[0].id.clone());
        recipe.masks.push(future);
        let mut patch = Map::new();
        patch.insert("x0".into(), json!(0.5));
        let error = plan(
            find("mask.set-component").unwrap(),
            &recipe,
            &MaskTarget {
                mask: Some(mask),
                component: Some(component),
                name: None,
            },
            &patch,
            &registry(),
        )
        .unwrap_err();
        assert_eq!(error.detail, "unknown mask component cloud");
        assert_eq!(error.kind, ErrorKind::Incompatible);
    }

    #[test]
    fn an_index_outside_the_list_it_addresses_is_refused_with_the_count() {
        let (recipe, _) = created();
        let mut index = Map::new();
        index.insert("index".into(), json!(3));
        assert_eq!(
            plan(
                find("mask.reorder").unwrap(),
                &recipe,
                &MaskTarget {
                    mask: Some(recipe.masks[0].id.clone()),
                    ..MaskTarget::default()
                },
                &index,
                &registry()
            )
            .unwrap_err()
            .detail,
            "index 3 is outside the 1 masks of this stack"
        );
    }

    #[test]
    fn the_schema_of_each_command_states_its_envelope_and_its_parameters() {
        let create = find("mask.create").unwrap().schema();
        assert_eq!(create["mutates"], json!(true));
        assert_eq!(
            create["required"],
            json!(["asset_id", "mutation", "kind", "x0", "y0", "x1", "y1"])
        );
        let patch = find("mask.set-component").unwrap().schema();
        assert_eq!(patch["patch"], json!(true));
        assert_eq!(
            patch["required"],
            json!(["asset_id", "mutation", "mask", "component"])
        );
        assert_eq!(
            patch["optional"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["x0", "x1", "y0", "y1"],
            "every field of a patch is optional, whatever it declares"
        );
        let list = find(LIST).unwrap().schema();
        assert_eq!(list["mutates"], json!(false));
        assert_eq!(list["required"], json!(["asset_id"]));
        assert_eq!(
            list["optional"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["entry_id"]
        );
        let rename = find("mask.rename").unwrap().schema();
        assert_eq!(
            rename["required"],
            json!(["asset_id", "mutation", "mask", "name"])
        );
    }

    #[test]
    fn the_generic_parameter_check_is_the_only_path_a_value_takes() {
        let create = &find("mask.create").unwrap().action;
        // Out of range, wrong type, unknown and missing, all refused by the delivered check.
        assert_eq!(
            crate::check_parameters(
                create,
                &json!({"kind":"linear","x0":3.0,"y0":0,"x1":0,"y1":1})
            )
            .unwrap_err()
            .detail,
            "parameter x0 must be a number within -1..=2"
        );
        assert_eq!(
            crate::check_parameters(
                create,
                &json!({"kind":"radial","x0":0,"y0":0,"x1":0,"y1":1})
            )
            .unwrap_err()
            .detail,
            "parameter kind must be one of linear"
        );
        assert_eq!(
            crate::check_parameters(
                create,
                &json!({"kind":"linear","mode":"add","x0":0,"y0":0,"x1":0,"y1":1})
            )
            .unwrap_err()
            .detail,
            "unknown parameter mode for action mask.create",
            "the first component of a mask is always add, so no mode can be requested"
        );
        assert_eq!(
            crate::check_parameters(create, &json!({"kind":"linear","x0":0,"y0":0,"x1":0}))
                .unwrap_err()
                .detail,
            "missing required parameter y1 for action mask.create"
        );
        // A patch fills no defaults and demands nothing.
        let patch = &find("mask.set-component").unwrap().action;
        assert_eq!(
            crate::check_parameters(patch, &json!({"y1":0.5})).unwrap(),
            {
                let mut fields = Map::new();
                fields.insert("y1".into(), json!(0.5));
                fields
            }
        );
    }
}
