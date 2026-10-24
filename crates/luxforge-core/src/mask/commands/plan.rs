//! What one command does to a stack: the family's behaviour, matched on each command's [`MaskOp`],
//! and the history label it commits.
use super::{
    GeometryMethod, GeometryOp, MaskCommand, MaskOp, MaskTarget, PickMethod, RemovedLayer,
    SampleMethod, SampleOp, spoken,
};
use crate::mask::{
    Picked, SAMPLES_FIELD, component_geometry_is_drawn, component_parameters, component_pick,
    component_sample_limit, component_sample_parameters, knows_component_kind, rules, stroke_kind,
};
use crate::{
    Component, ComponentId, ComponentMode, EffectStage, Error, Layer, LayerId, Mask, MaskId,
    ModuleRegistry, Recipe,
    mask::{ColourLimit, Stroke},
    model::MASKS_PER_RECIPE,
    path::{self, StrokeId},
    tiles::InputSite,
};
use serde_json::{Map, Value, json};

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

/// What one command touched, before the one label rule ([`mask_label`]) is applied to it.
#[derive(Debug, Default)]
struct Planned {
    /// The command's own history text, before any mask prefix.
    label: String,
    /// Whether `label` already names its mask, so the prefix would say it twice.
    names_mask: bool,
    /// The mask the command addressed, or made.
    mask: Option<MaskId>,
    /// The component the command addressed, made or removed.
    component: Option<ComponentId>,
    /// The layers a destructive command removed with its mask.
    removed_layers: Vec<RemovedLayer>,
}

/// What the host read for one command before it is planned, because [`plan`] reads no pixel: the
/// pixel a colour-limited stroke is seeded on, as the three sRGB codes read at the site and position
/// [`colour_limit_request`] named, or what a pick read at the site and place [`pick_request`] named.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum HostRead<'a> {
    /// The command reads nothing.
    #[default]
    Nothing,
    Seed([u8; 3]),
    Picked(Picked<'a>),
}

/// What one command would do to this stack: the whole family's behaviour in one place.
///
/// Pure — it reads the recipe it is handed and returns a new one — so the same function answers a
/// commit and a drafted preview, and a drafted gesture therefore previews exactly the stack
/// committing it would write. It reads no pixels and costs `O(masks + components + layers)`, plus a
/// pick's `O(points)` over at most [`super::AREA_POINTS`].
///
/// `read` is what the host read for the command beforehand: a limited stroke's seed or a pick's
/// pixels. It arrives as an argument rather than being read here because this function reads no
/// pixels — that is what lets a drafted gesture preview exactly the stack its release commits,
/// through the same call.
pub(crate) fn plan(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    registry: &ModuleRegistry,
    read: HostRead<'_>,
) -> Result<MaskOutcome, Error> {
    let seed = match read {
        HostRead::Seed(seed) => Some(seed),
        HostRead::Nothing | HostRead::Picked(_) => None,
    };
    let mut next = recipe.clone();
    // Each arm produces what the command touched and its base label. The mask prefix is applied
    // once, below, so there is one label rule.
    //
    // Every command is dispatched by what it does, never by its spelling, and the match is
    // exhaustive. A generated geometry method carries the kind it was generated for:
    // `mask.create-radial` reaches the same three arms `mask.create-linear` does, and a kind
    // registered later reaches them without this function learning its name.
    let planned = match command.op {
        MaskOp::Geometry(geometry) => plan_geometry(geometry, &mut next, target, parameters)?,
        MaskOp::Sample(samples) => plan_sample(samples, &mut next, target, parameters)?,
        MaskOp::Pick(pick) => plan_pick(pick, &mut next, target, parameters, read)?,
        MaskOp::AddStroke => plan_add_stroke(&mut next, target, parameters, seed)?,
        MaskOp::DeleteStroke => plan_delete_stroke(&mut next, target)?,
        MaskOp::Delete => {
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
            let label = match spoken_titles(&removed) {
                Some(titles) => format!("Delete {} with {titles}", mask.name),
                None => format!("Delete {}", mask.name),
            };
            Planned {
                label,
                names_mask: true,
                mask: Some(mask.id),
                removed_layers: removed,
                ..Planned::default()
            }
        }
        MaskOp::Rename => {
            let index = mask_index(&next, required_mask(target)?)?;
            let name = target
                .name
                .clone()
                .ok_or_else(|| Error::validation("missing required field name for mask.rename"))?;
            let previous = std::mem::replace(&mut next.masks[index].name, name.clone());
            next.masks[index].validate()?;
            Planned {
                label: format!("Rename {previous} to {name}"),
                names_mask: true,
                mask: Some(next.masks[index].id.clone()),
                ..Planned::default()
            }
        }
        MaskOp::RenameComponent => {
            let (mask_index, index) = component_at(&next, target)?;
            let name = target.name.clone().ok_or_else(|| {
                Error::validation("missing required field name for mask.rename-component")
            })?;
            let mask = &mut next.masks[mask_index];
            let previous = std::mem::replace(&mut mask.components[index].name, name.clone());
            let component = mask.components[index].id.clone();
            // Structural only, exactly as a mask's own rename: printable, trimmed and not empty,
            // and — unlike a mask's name — unique within this mask, because that is the uniqueness
            // `Mask::validate` already enforces over every component's name.
            mask.validate()?;
            Planned {
                label: format!("Rename {previous} to {name}"),
                mask: Some(mask.id.clone()),
                component: Some(component),
                ..Planned::default()
            }
        }
        MaskOp::Duplicate => {
            rules::room_for_mask(next.masks.len())?;
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
            // A mask without its adjustments is not a useful copy, so the layers bound to the source
            // are copied with it, each with a new identity and bound to the copy.
            //
            // The copies are legal because `single_layer` is per *target* and the global layer and
            // each mask are distinct targets (`docs/design/masking.md`, "How a mask reaches an
            // effect"): a second masked Basic layer bound to a different mask is a second target, not
            // an ambiguous duplicate, and `ModuleRegistry::compile_layers` checks exactly that pair.
            //
            // They are placed by the ordering rule rather than sorted into it afterwards. The copy
            // sits at `index + 1`, immediately after its source, so a copied layer placed immediately
            // after the layer it was copied from is already after the global layer of its effect and
            // already in mask order among the masked layers of that effect — the two clauses of the
            // rule, satisfied by construction and inside the source layer's own stage region.
            let copied = next
                .layers
                .iter()
                .filter(|layer| layer.mask.as_ref() == Some(&source.id))
                .count();
            if copied > 0 {
                let mut layers = Vec::with_capacity(next.layers.len() + copied);
                for layer in &next.layers {
                    layers.push(layer.clone());
                    if layer.mask.as_ref() == Some(&source.id) {
                        layers.push(Layer {
                            id: LayerId::new(),
                            mask: Some(id.clone()),
                            ..layer.clone()
                        });
                    }
                }
                next.layers = layers;
            }
            Planned {
                label: format!("Duplicate {}", source.name),
                names_mask: true,
                mask: Some(id),
                ..Planned::default()
            }
        }
        MaskOp::SetAmount => {
            let index = mask_index(&next, required_mask(target)?)?;
            let amount = number(parameters, "amount")?;
            next.masks[index].amount = amount;
            next.masks[index].validate()?;
            Planned {
                label: format!("Amount {amount}"),
                mask: Some(next.masks[index].id.clone()),
                ..Planned::default()
            }
        }
        MaskOp::SetInvert => {
            let index = mask_index(&next, required_mask(target)?)?;
            let invert = boolean(parameters, "invert")?;
            next.masks[index].invert = invert;
            Planned {
                label: inversion_label(invert).to_owned(),
                mask: Some(next.masks[index].id.clone()),
                ..Planned::default()
            }
        }
        MaskOp::Reorder => {
            let index = mask_index(&next, required_mask(target)?)?;
            let to = position(parameters, "index", next.masks.len(), "masks")?;
            let mask = next.masks.remove(index);
            let (id, name) = (mask.id.clone(), mask.name.clone());
            next.masks.insert(to, mask);
            // Masked layers of one effect are evaluated in their masks' order, so moving a mask
            // moves them with it, in this one transaction, and nothing else moves.
            //
            // The rule lives beside the placement rule it is the other half of, in the registry, and
            // this is its one call site. There is no second re-sort here: an earlier copy in this
            // module predated the placement rule and permuted only the positions masked layers
            // already held, which left a masked layer that should have followed a *global* layer of
            // its effect where it was.
            registry.sort_masked_layers(&mut next.layers, &next.masks);
            Planned {
                label: format!("Move {name} to {}", to + 1),
                names_mask: true,
                mask: Some(id),
                ..Planned::default()
            }
        }
        MaskOp::SetComponentMode => {
            let (mask_index, index) = component_at(&next, target)?;
            let mode = mode(parameters)?;
            let mask = &mut next.masks[mask_index];
            mask.components[index].mode = mode;
            let label = format!("{} {}", mask.components[index].name, mode.as_str());
            let component = mask.components[index].id.clone();
            // The first component of a mask is always add, so promoting one to subtract or
            // intersect is refused here with the model's own reason.
            mask.validate()?;
            Planned {
                label,
                mask: Some(mask.id.clone()),
                component: Some(component),
                ..Planned::default()
            }
        }
        MaskOp::SetComponentInvert => {
            let (mask_index, index) = component_at(&next, target)?;
            let invert = boolean(parameters, "invert")?;
            let mask = &mut next.masks[mask_index];
            mask.components[index].invert = invert;
            Planned {
                label: format!(
                    "{} {}",
                    mask.components[index].name,
                    inversion_label(invert).to_lowercase()
                ),
                mask: Some(mask.id.clone()),
                component: Some(mask.components[index].id.clone()),
                ..Planned::default()
            }
        }
        MaskOp::DeleteComponent => {
            let (mask_index, index) = component_at(&next, target)?;
            let mask = &mut next.masks[mask_index];
            // A mask never exists empty from a command, so its last component is not deletable:
            // deleting the mask is the command that removes it, and it says what it removed.
            rules::delete_component(&mask.name, mask.components.len())?;
            let removed = mask.components.remove(index);
            // Removing the leading add component of a mask whose next component subtracts leaves a
            // mask that cannot be read; it is refused with the model's reason rather than promoted.
            mask.validate()?;
            Planned {
                label: format!("Delete {}", removed.name),
                mask: Some(mask.id.clone()),
                component: Some(removed.id),
                ..Planned::default()
            }
        }
        MaskOp::ReorderComponent => {
            let (mask_index, index) = component_at(&next, target)?;
            let mask = &mut next.masks[mask_index];
            let to = requested_index(parameters, "index")?;
            let modes: Vec<ComponentMode> = mask
                .components
                .iter()
                .map(|component| component.mode)
                .collect();
            // The destination and the component the move would leave leading are the one rule a
            // panel states before offering the move.
            rules::reorder_component(&mask.name, &modes, index, to)?;
            let to = rules::position(to, modes.len(), "components")?;
            let component = mask.components.remove(index);
            let (component_id, name) = (component.id.clone(), component.name.clone());
            mask.components.insert(to, component);
            mask.validate()?;
            Planned {
                label: format!("Move {name}"),
                mask: Some(mask.id.clone()),
                component: Some(component_id),
                ..Planned::default()
            }
        }
    };
    // Nothing changed: the value was already that, or a drag returned to where it began. No entry
    // and no event, exactly as a module's no-op.
    if next == *recipe {
        return Ok(MaskOutcome::NoOp);
    }
    let label = mask_label(
        planned.label,
        planned.names_mask,
        planned.mask.as_ref(),
        &next,
    );
    Ok(MaskOutcome::Change(MaskChange {
        recipe: next,
        label,
        mask: planned.mask,
        component: planned.component,
        removed_layers: planned.removed_layers,
    }))
}

/// The three generated geometry methods, over whichever kind the method was generated for.
///
/// Nothing here names a component kind. `geometry.kind` came from the host's kind table when the
/// method was generated, the parameters the request carries were already validated against that
/// kind's own declarations by the generic check, and the payload is built from the names of those
/// same declarations — so a kind registered later reaches all three arms unchanged.
fn plan_geometry(
    geometry: GeometryMethod,
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
) -> Result<Planned, Error> {
    let kind = geometry.kind;
    match geometry.op {
        GeometryOp::Create => {
            rules::room_for_mask(next.masks.len())?;
            let mut mask = Mask::new(next_mask_name(next));
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
            Ok(Planned {
                label: format!("Add {}", spoken_geometry(kind, parameters)),
                mask: Some(mask_id),
                component: Some(component_id),
                ..Planned::default()
            })
        }
        GeometryOp::Add => {
            let index = mask_index(next, required_mask(target)?)?;
            let mask = &mut next.masks[index];
            rules::room_for_component(&mask.name, mask.components.len())?;
            let mode = mode(parameters)?;
            let name = mask.next_component_name(kind);
            let component = Component::new(name, mode, kind, geometry_payload(kind, parameters)?);
            let component_id = component.id.clone();
            mask.components.push(component);
            // The first component of a mask is always add, so a subtract or an intersect arriving at
            // an empty mask is refused here with the model's own reason rather than silently
            // creating a selection of nothing.
            mask.validate()?;
            let base = if mode == ComponentMode::Add {
                format!("Add {}", spoken_geometry(kind, parameters))
            } else {
                format!(
                    "Add {} {}",
                    mode.as_str(),
                    spoken_geometry(kind, parameters)
                )
            };
            let id = mask.id.clone();
            Ok(Planned {
                label: base,
                mask: Some(id),
                component: Some(component_id),
                ..Planned::default()
            })
        }
        GeometryOp::Set => {
            let (mask_index, index) = component_at(next, target)?;
            let mask = &mut next.masks[mask_index];
            let component = &mut mask.components[index];
            // A component whose kind this build cannot evaluate is `incompatible` and not a mismatch:
            // there is no generated method to point at, the stack is well formed, and the honest
            // answer is that this build cannot read that component at all. It is the same refusal
            // rendering gives, in the same spelling.
            if !knows_component_kind(&component.kind) {
                return Err(rules::unknown_kind(&component.kind));
            }
            // One *known* kind's patch may not reach another known kind's component. Both are named,
            // because a client that picked the wrong generated method has to be told which one to
            // use.
            if component.kind != kind {
                // A kind whose geometry is drawn has no patch method to point at, so the refusal
                // says what is true of it rather than naming a command that does not exist.
                if component_geometry_is_drawn(&component.kind) {
                    return Err(Error::validation(format!(
                        "component {} is a {} component, whose geometry is drawn rather than \
                         patched",
                        component.name, component.kind
                    )));
                }
                return Err(Error::validation(format!(
                    "component {} is a {} component; patch it with mask.set-{}",
                    component.name, component.kind, component.kind
                )));
            }
            let mut payload = component
                .payload
                .as_object()
                .cloned()
                .ok_or_else(|| rules::unknown_kind(&component.kind))?;
            for (field, value) in parameters {
                // Only an optional field of a kind's own declaration may be null (the generic
                // check refuses one anywhere else), and a null clears it.
                if value.is_null() {
                    payload.remove(field);
                } else {
                    payload.insert(field.clone(), canonical(value));
                }
            }
            component.payload = Value::Object(payload);
            // A later edit to a component names that component, so the row says which one it was.
            let base = format!("Update {}", component.name);
            let component_id = component.id.clone();
            mask.validate()?;
            let id = mask.id.clone();
            Ok(Planned {
                label: base,
                mask: Some(id),
                component: Some(component_id),
                ..Planned::default()
            })
        }
    }
}

/// `mask.add-stroke`: one painted stroke, and whichever of the three edits its identities say it
/// is.
///
/// The stroke is captured **before** anything is decided, so a malformed path refuses without having
/// touched the mask table, and it is put in the recipe's own stroke table under its content address.
/// **No coordinate is written into a component payload**: the payload carries the reserved `strokes`
/// field holding addresses, which is what keeps one entry per stroke from copying every earlier
/// stroke of that component into every later entry.
///
/// Capture is the host's, not the desktop's. The desktop decimates before it posts, on the same
/// grid at the same tolerance, and that is idempotent — so an agent that posts a raw path and a hand
/// that drew one reach the same stored bytes, the same address and the same coverage.
fn plan_add_stroke(
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    seed: Option<[u8; 3]>,
) -> Result<Planned, Error> {
    let stroke = captured_stroke(parameters)?;
    // A limit is stored with the stroke, from the colour the host read where the stroke began: never
    // from the request, and never read again when the picture is drawn.
    let stroke = match (boolean(parameters, "limit_to_colour")?, seed) {
        (false, _) => stroke,
        (true, Some(seed)) => stroke.with_colour_limit(ColourLimit::sampled(
            seed,
            number(parameters, "colour_refine")?,
        )?),
        (true, None) => {
            return Err(Error::validation(
                "a stroke limited to a colour needs the pixel the masked operation receives, and \
                 none was read",
            ));
        }
    };
    let id = next.strokes.insert(stroke);
    // The mode belongs to a component, and only a stroke that makes one may carry it. Appending to a
    // component that already exists is refused rather than silently ignoring the mode, and the
    // refusal names the command that does change one.
    let declared_mode = || -> Result<(), Error> {
        if parameters.contains_key("mode") {
            return Err(Error::validation(
                "a stroke appended to an existing component takes no mode; change a component's \
                 mode with mask.set-component-mode",
            ));
        }
        Ok(())
    };
    match (&target.mask, &target.component) {
        // The first stroke of a session: a mask, a brush component and the stroke, as one entry.
        (None, _) => {
            declared_mode()?;
            rules::room_for_mask(next.masks.len())?;
            let mut mask = Mask::new(next_mask_name(next));
            let name = mask.next_component_name(stroke_kind());
            // The first component of a mask is always add, exactly as `mask.create-<kind>` makes it.
            let component = Component::new(
                name,
                ComponentMode::Add,
                stroke_kind(),
                strokes_payload(&[id]),
            );
            let component_id = component.id.clone();
            let mask_id = mask.id.clone();
            mask.components.push(component);
            mask.validate()?;
            next.masks.push(mask);
            Ok(Planned {
                label: format!("Add {}", spoken(stroke_kind())),
                mask: Some(mask_id),
                component: Some(component_id),
                ..Planned::default()
            })
        }
        // A further brush on a mask that exists, in the mode the gesture chose before it started.
        (Some(_), None) => {
            let index = mask_index(next, required_mask(target)?)?;
            let mode = optional_mode(parameters)?.unwrap_or(ComponentMode::Add);
            let mask = &mut next.masks[index];
            rules::room_for_component(&mask.name, mask.components.len())?;
            let name = mask.next_component_name(stroke_kind());
            let component = Component::new(name, mode, stroke_kind(), strokes_payload(&[id]));
            let component_id = component.id.clone();
            mask.components.push(component);
            mask.validate()?;
            let base = if mode == ComponentMode::Add {
                format!("Add {}", spoken(stroke_kind()))
            } else {
                format!("Add {} {}", mode.as_str(), spoken(stroke_kind()))
            };
            let mask_id = mask.id.clone();
            Ok(Planned {
                label: base,
                mask: Some(mask_id),
                component: Some(component_id),
                ..Planned::default()
            })
        }
        // Every later stroke on that component: one entry each, named for the component, which is
        // what makes undo walk back one stroke at a time with no second history model.
        (Some(_), Some(_)) => {
            declared_mode()?;
            let (mask_index, index) = component_at(next, target)?;
            let mask = &mut next.masks[mask_index];
            let mask_name = mask.name.clone();
            let component = &mut mask.components[index];
            strokes_reach(component)?;
            let mut held = path::references(
                &component.payload,
                &format!("component {} of mask {mask_name}", component.name),
            )?;
            held.push(id);
            component.payload = strokes_payload(&held);
            let base = format!("Update {}", component.name);
            let component_id = component.id.clone();
            mask.validate()?;
            let mask_id = mask.id.clone();
            Ok(Planned {
                label: base,
                mask: Some(mask_id),
                component: Some(component_id),
                ..Planned::default()
            })
        }
    }
}

/// `mask.delete-stroke`: a **forward edit**, not an undo.
///
/// It removes one stroke and appends one entry, so a stroke made ten entries ago goes while
/// everything after it stays; `history.undo` still walks entries, and the two never mean the same
/// thing. Deleting is well defined because the fold is over the stored order: the remaining strokes
/// produce, bit for bit, the field they would have produced had the deleted one never been made.
///
/// A component holding the same address twice holds two strokes with the same content — painting one
/// path twice builds up, and the second pass is a second object — so this removes the **first** of
/// them, which is the one earliest in the fold.
fn plan_delete_stroke(next: &mut Recipe, target: &MaskTarget) -> Result<Planned, Error> {
    let wanted = target
        .stroke
        .as_ref()
        .ok_or_else(|| Error::validation("missing required field stroke"))?
        .clone();
    let (mask_index, index) = component_at(next, target)?;
    let mask = &mut next.masks[mask_index];
    let mask_name = mask.name.clone();
    let component = &mut mask.components[index];
    strokes_reach(component)?;
    let mut held = path::references(
        &component.payload,
        &format!("component {} of mask {mask_name}", component.name),
    )?;
    let Some(at) = held.iter().position(|held| held == &wanted) else {
        return Err(Error::validation(format!(
            "component {} holds no stroke {wanted}",
            component.name
        )));
    };
    // A component with no stroke covers nothing and is not a thing a person drew, so the last stroke
    // is removed by removing the component — the same rule, and the same wording, that keeps a mask
    // from existing empty.
    rules::delete_stroke(wanted.as_str(), &component.name, held.len())?;
    held.remove(at);
    component.payload = strokes_payload(&held);
    let base = format!("Delete a stroke from {}", component.name);
    let component_id = component.id.clone();
    mask.validate()?;
    let mask_id = mask.id.clone();
    Ok(Planned {
        label: base,
        mask: Some(mask_id),
        component: Some(component_id),
        ..Planned::default()
    })
}

/// The component a stroke may reach: one whose geometry is drawn as a path, which the kind table
/// answers ([`component_geometry_is_drawn`]) so the command family matches on no kind of its own.
///
/// A component of a kind this build cannot evaluate is `incompatible`, exactly as rendering it is; a
/// known kind whose geometry is declared numbers is a `validation` refusal naming the patch method
/// that does edit it, because a client that picked the wrong command has to be told the right one.
fn strokes_reach(component: &Component) -> Result<(), Error> {
    if !knows_component_kind(&component.kind) {
        return Err(rules::unknown_kind(&component.kind));
    }
    if !component_geometry_is_drawn(&component.kind) {
        return Err(Error::validation(format!(
            "component {} is a {} component, whose geometry is declared rather than drawn; patch it \
             with mask.set-{}",
            component.name, component.kind, component.kind
        )));
    }
    Ok(())
}

/// A brush component's stored payload: the host's reserved `strokes` field holding its strokes in
/// order by content address, and nothing else.
pub(super) fn strokes_payload(strokes: &[StrokeId]) -> Value {
    json!({ path::STROKES_FIELD: strokes })
}

/// Where a mask's value-based parts read the pixel the operation it modulates receives: the
/// position in the stack, and how the operation there receives its input.
///
/// **The rule, stated once for everything that reads a pixel through a mask** — the coverage
/// overlay and thumbnail, `mask.sample-input` and a colour-limited stroke's seed:
///
/// - A mask a layer is bound to reads the input of the **first layer bound to it in evaluation
///   order**, as that layer receives it. A mask several layers at different stages share therefore
///   has one answer, the earliest operation it modulates.
/// - A mask no layer is bound to reads where **a first adjustment through it would go**: the
///   earliest position a maskable colour effect — Basic's, in practice — would take for this mask
///   by the placement rule a commit uses ([`ModuleRegistry::insertion_index_for_target`]), read as
///   a colour operation receives it. The mode is stated rather than asked of the layer that holds
///   that position now, which may be a spatial layer or none at all. So a range drawn before any
///   adjustment selects what it selects once Basic is applied through it; a first Curve, Mixer,
///   Presence or Detail adjustment may move the selection, because the first bound layer then
///   decides. Only colour effects are asked, on purpose: Detail, at the restoration stage, would
///   place itself ahead of a global Basic layer, and binding a first Basic adjustment would then
///   move the site past that layer and the selection with it. A host that registers no maskable
///   colour effect reads where its earliest maskable effect would go, as that effect receives its
///   input.
/// - A mask the stack does not hold yet — the mask a stroke is about to draw — sorts after every
///   mask it does hold, as a new last mask does.
///
/// Only a host with no maskable effect at all is refused ([`rules::maskable_operation`]). Reading
/// the source or the finished frame instead would put the pixel in a different domain from the
/// one the selection is evaluated in: the [range study](../../../../../docs/design/range-study.md)
/// measures a `+0.75 EV` layer ahead of a band taking a sky from fully selected to not selected at
/// all.
///
/// `O(layers · (maskable effects + masks))`; reads no pixels.
pub fn input_site(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    mask: &MaskId,
) -> Result<InputSite, Error> {
    if let Some(index) = recipe
        .layers
        .iter()
        .position(|layer| layer.mask.as_ref() == Some(mask))
    {
        return Ok(InputSite::of_layer(registry, recipe, index));
    }
    let earliest = |colour: bool| {
        registry
            .maskable_effects()
            .filter(|effect| !colour || effect.stage == EffectStage::Color)
            .map(|effect| {
                let index = registry.insertion_index_for_target(
                    &recipe.layers,
                    &effect.id,
                    Some(mask),
                    &recipe.masks,
                );
                (index, effect.stage)
            })
            .min_by_key(|(index, _)| *index)
    };
    let found = earliest(true).or_else(|| earliest(false));
    let name = recipe
        .masks
        .iter()
        .find(|held| &held.id == mask)
        .map_or_else(|| mask.as_str().to_owned(), |held| held.name.clone());
    rules::maskable_operation(&name, found.is_some())?;
    let (index, stage) = found.unwrap_or((recipe.layers.len(), EffectStage::Color));
    Ok(InputSite {
        index,
        mode: crate::render::MaskInputMode::for_stage(Some(stage)).into(),
    })
}

/// What one `mask.add-stroke` needs read before it can be planned, when it asks for a colour limit:
/// the layer whose input to read, the position to read it at, and the refine to compile with.
///
/// It is answered here, beside the command that asks for it, and the pixel itself is read by the
/// host — the split is deliberate. **The rule** (which layer, and what a stroke starting outside the
/// picture means) belongs with the command family; **the pixel** belongs to the editor, which is the
/// only thing that can evaluate one. So a client cannot supply a colour at any point: the request
/// carries a flag and a path, and the colour that ends up stored is the one the photograph has at
/// the position the stroke started from.
///
/// The position is the stroke's **stored** first position, not the raw one posted, so a hand-drawn
/// path and an agent's raw copy of it seed from the same pixel exactly as they store the same bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LimitRequest {
    /// Where the seed is read, by [`input_site`].
    pub site: InputSite,
    /// The stroke's first stored position, in the content stage's normalized coordinates.
    pub x: f64,
    pub y: f64,
    pub refine: f64,
}

/// Whether a mask command's `fields` ask for a colour limit, whose seed its plan reads from a
/// pixel: the one planning read a drafted gesture makes. Reads the flag alone.
pub(crate) fn asks_colour_limit(fields: &Map<String, Value>) -> bool {
    fields.get("limit_to_colour").and_then(Value::as_bool) == Some(true)
}

/// [`asks_colour_limit`] of a request's fields as a JSON value, as a client holds them: whether
/// planning them reads a pixel, which a client that must not wait on one sends where it may.
pub fn asks_colour_limit_value(fields: &Value) -> bool {
    fields.as_object().is_some_and(asks_colour_limit)
}

/// Whether this request asks for a colour limit, and everything the host needs to seed it.
pub(crate) fn colour_limit_request(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    registry: &ModuleRegistry,
) -> Result<Option<LimitRequest>, Error> {
    if command.op != MaskOp::AddStroke || !boolean(parameters, "limit_to_colour")? {
        return Ok(None);
    }
    // A stroke with no mask draws a new one, which no layer is bound to and which joins the list
    // last: an identity the stack does not hold is placed exactly so.
    let drawn;
    let mask = match target.mask.as_ref() {
        Some(mask) => mask,
        None => {
            drawn = MaskId::new();
            &drawn
        }
    };
    let site = input_site(registry, recipe, mask)?;
    // The stored first position, which is what makes the seed a property of the stroke the store
    // holds rather than of the raw path one client happened to post.
    let stroke = captured_stroke(parameters)?;
    let [x, y] = stroke
        .points()
        .next()
        .ok_or_else(|| Error::validation("a stroke has no position to read a colour at"))?;
    Ok(Some(LimitRequest {
        site,
        x,
        y,
        refine: number(parameters, "colour_refine")?,
    }))
}

/// What one `mask.pick-<kind>` needs read before it can be planned: the mask's input site, the
/// picked point in the content stage's normalized coordinates, and the dragged rectangle's size when
/// the pick is a drag.
///
/// The component it fills is checked first — that it exists, that this build knows its kind and
/// that the kind is the one the method was generated for — so a pick that would be refused is
/// refused before anything is read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PickRequest {
    pub mask: MaskId,
    pub site: InputSite,
    pub x: f64,
    pub y: f64,
    /// `(width, height)` of a drag; `None` for a click.
    pub area: Option<(f64, f64)>,
}

/// Whether this request is a pick, and what the host reads to plan it.
pub(crate) fn pick_request(
    command: &MaskCommand,
    recipe: &Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    registry: &ModuleRegistry,
) -> Result<Option<PickRequest>, Error> {
    let MaskOp::Pick(pick) = command.op else {
        return Ok(None);
    };
    let (mask_index, index) = component_at(recipe, target)?;
    picked_component(&recipe.masks[mask_index].components[index], pick)?;
    let mask = recipe.masks[mask_index].id.clone();
    let area = match (parameters.get("width"), parameters.get("height")) {
        (None, None) => None,
        (Some(_), Some(_)) => Some((number(parameters, "width")?, number(parameters, "height")?)),
        _ => {
            return Err(Error::validation(
                "a dragged area names both width and height; a click names neither",
            ));
        }
    };
    Ok(Some(PickRequest {
        site: input_site(registry, recipe, &mask)?,
        mask,
        x: number(parameters, "x")?,
        y: number(parameters, "y")?,
        area,
    }))
}

/// A component `pick` may fill: one of a kind this build knows, and the kind the method was
/// generated for. An unknown kind is `incompatible`, as rendering it is; another kind is refused
/// naming the method that does pick it, when there is one.
fn picked_component(component: &Component, pick: PickMethod) -> Result<(), Error> {
    if !knows_component_kind(&component.kind) {
        return Err(rules::unknown_kind(&component.kind));
    }
    if component.kind != pick.kind {
        let remedy = match super::declare::pick(&component.kind) {
            Some(command) => format!("; pick it with {}", command.method),
            None => format!(
                "; a {} component is not picked from the photograph",
                spoken(&component.kind)
            ),
        };
        return Err(Error::validation(format!(
            "component {} is a {} component, not a {}{remedy}",
            component.name,
            spoken(&component.kind),
            spoken(pick.kind)
        )));
    }
    Ok(())
}

/// The generated pick method, over whichever kind it was generated for: the kind's own pick rule
/// turns what the host read into the component's new payload. One entry, `Pick` and the component's
/// name; a pick that leaves the payload as it was — a colour already held — writes none.
fn plan_pick(
    pick: PickMethod,
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
    read: HostRead<'_>,
) -> Result<Planned, Error> {
    let HostRead::Picked(picked) = read else {
        return Err(Error::validation(
            "a pick needs the pixels the masked operation receives, and none were read",
        ));
    };
    let rule = component_pick(pick.kind).ok_or_else(|| rules::unknown_kind(pick.kind))?;
    let (mask_index, index) = component_at(next, target)?;
    let mask = &mut next.masks[mask_index];
    let component = &mut mask.components[index];
    picked_component(component, pick)?;
    let add = parameters
        .get("add")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    component.payload = rule(component, picked, add)?;
    let label = format!("Pick {}", component.name);
    let component_id = component.id.clone();
    mask.validate()?;
    Ok(Planned {
        label,
        mask: Some(mask.id.clone()),
        component: Some(component_id),
        ..Planned::default()
    })
}

/// The stroke one `mask.add-stroke` posted, captured on the stored grid.
///
/// The generic parameter check has already refused a path that is not a list of in-range positions
/// and a setting outside its declared range; what happens here is the capture itself — snapping,
/// decimation and the per-stroke bound — which is [`crate::path`]'s contract and not a second copy
/// of it.
fn captured_stroke(parameters: &Map<String, Value>) -> Result<Stroke, Error> {
    Stroke::capture(
        &points(parameters, "points")?,
        number(parameters, "size")?,
        number(parameters, "feather")?,
        number(parameters, "flow")?,
        boolean(parameters, "erase")?,
    )
}

/// A checked `points` parameter as the pairs it declares.
fn points(parameters: &Map<String, Value>, name: &str) -> Result<Vec<[f64; 2]>, Error> {
    let listed = parameters
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::validation(format!("missing required parameter {name}")))?;
    listed
        .iter()
        .map(|point| {
            let pair = point.as_array().ok_or_else(|| {
                Error::validation(format!(
                    "parameter {name} must be a list of [x, y] positions"
                ))
            })?;
            match (
                pair.first().and_then(Value::as_f64),
                pair.get(1).and_then(Value::as_f64),
            ) {
                (Some(x), Some(y)) if pair.len() == 2 => Ok([x, y]),
                _ => Err(Error::validation(format!(
                    "parameter {name} must be a list of [x, y] positions"
                ))),
            }
        })
        .collect()
}

/// The mode a request carried, or none when it carried none. Unlike [`mode`], absence is an answer
/// rather than a refusal: the one command that declares an optional mode does something different
/// without it.
fn optional_mode(parameters: &Map<String, Value>) -> Result<Option<ComponentMode>, Error> {
    if !parameters.contains_key("mode") {
        return Ok(None);
    }
    mode(parameters).map(Some)
}

/// The two generated sample methods, over whichever sampling kind the method was generated for.
///
/// Nothing here names a component kind either: `samples.kind` came from the host's kind table when
/// the method was generated, the parameters were validated against that kind's own sample
/// declarations by the generic check, and the list is read and written through the reserved
/// `samples` field every sampling kind's payload carries.
fn plan_sample(
    samples: SampleMethod,
    next: &mut Recipe,
    target: &MaskTarget,
    parameters: &Map<String, Value>,
) -> Result<Planned, Error> {
    let kind = samples.kind;
    let limit = component_sample_limit(kind).ok_or_else(|| rules::unknown_kind(kind))?;
    let (mask_index, index) = component_at(next, target)?;
    let mask = &mut next.masks[mask_index];
    let component = &mut mask.components[index];
    if !knows_component_kind(&component.kind) {
        return Err(rules::unknown_kind(&component.kind));
    }
    if component.kind != kind {
        return Err(Error::validation(format!(
            "component {} is a {} component and holds no sampled colours of a {kind}",
            component.name, component.kind
        )));
    }
    let mut payload = component
        .payload
        .as_object()
        .cloned()
        .ok_or_else(|| rules::unknown_kind(&component.kind))?;
    let mut stored: Vec<Value> = payload
        .get(SAMPLES_FIELD)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let base = match samples.op {
        SampleOp::Add => {
            let picked = Value::Array(
                component_sample_parameters(kind)
                    .ok_or_else(|| rules::unknown_kind(kind))?
                    .iter()
                    .map(|declared| {
                        parameters
                            .get(&declared.name)
                            .map(canonical)
                            .ok_or_else(|| {
                                Error::validation(format!(
                                    "missing required parameter {} for a {kind} sample",
                                    declared.name
                                ))
                            })
                    })
                    .collect::<Result<Vec<Value>, Error>>()?,
            );
            // Sampling a colour the component already holds is a no-op, because the selection folds
            // its samples by nearest and a duplicate changes no pixel's coverage. Storing it anyway
            // would spend one of the component's few swatches on nothing.
            if !stored.contains(&picked) {
                rules::room_for_sample(&component.name, kind, stored.len(), limit)?;
                stored.push(picked);
            }
            format!("Sample {}", component.name)
        }
        SampleOp::Delete => {
            let at = position(parameters, "index", stored.len(), "sampled colours")?;
            stored.remove(at);
            format!("Remove a sample from {}", component.name)
        }
    };
    payload.insert(SAMPLES_FIELD.to_owned(), Value::Array(stored));
    component.payload = Value::Object(payload);
    let component_id = component.id.clone();
    mask.validate()?;
    let id = mask.id.clone();
    Ok(Planned {
        label: base,
        mask: Some(id),
        component: Some(component_id),
        ..Planned::default()
    })
}

/// The one label rule: the command's own text, prefixed with the mask's name whenever the resulting
/// stack carries more than one mask, because a history list shared with every other module cannot
/// afford `Update Linear 1` alone.
///
/// It is evaluated on the *resulting* mask table, so the mask a `mask.create` just made is counted;
/// and a command whose text already names its mask — rename, duplicate, reorder and the destructive
/// delete — is not prefixed with it twice.
/// A component a geometry method adds, as its history label says it: its kind (`linear`), or for a
/// model selection the class it selected and that a model selected it (`object (model)`), so a
/// picked object's entry reads `Add object (model)` and a sky's `Add sky (model)`.
fn spoken_geometry(kind: &str, parameters: &Map<String, Value>) -> String {
    match parameters.get("class").and_then(Value::as_str) {
        Some(class) if kind == crate::mask::model::KIND => format!("{class} (model)"),
        _ => spoken(kind),
    }
}

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

fn required_mask(target: &MaskTarget) -> Result<&MaskId, Error> {
    target
        .mask
        .as_ref()
        .ok_or_else(|| Error::validation("missing required field mask"))
}

fn mask_index(recipe: &Recipe, id: &MaskId) -> Result<usize, Error> {
    recipe
        .masks
        .iter()
        .position(|mask| &mask.id == id)
        .ok_or_else(|| Error::validation(format!("unknown mask {id}")))
}

/// The mask and the component one component command addresses.
fn component_at(recipe: &Recipe, target: &MaskTarget) -> Result<(usize, usize), Error> {
    let mask = mask_index(recipe, required_mask(target)?)?;
    let id = target
        .component
        .as_ref()
        .ok_or_else(|| Error::validation("missing required field component"))?;
    let index = recipe.masks[mask]
        .components
        .iter()
        .position(|component| &component.id == id)
        .ok_or_else(|| {
            Error::validation(format!(
                "mask {} has no component {id}",
                recipe.masks[mask].name
            ))
        })?;
    Ok((mask, index))
}

/// The declared payload fields of one component kind: the names of the parameters that kind's own
/// module declares, so a geometry parameter and a stored field are one spelling because they are one
/// declaration.
fn geometry_fields(kind: &str) -> Option<Vec<(String, bool)>> {
    component_parameters(kind, true).map(|parameters| {
        parameters
            .into_iter()
            .map(|parameter| (parameter.name, parameter.required))
            .collect()
    })
}

/// The stored payload of a new component of `kind`, built from that kind's declared geometry.
///
/// Numbers are canonicalized to `f64`, so a client that sends `0` where another sends `0.0` writes
/// the same bytes, and setting a field back to what it already held is recognized as a no-op.
fn geometry_payload(kind: &str, parameters: &Map<String, Value>) -> Result<Value, Error> {
    let fields = geometry_fields(kind).ok_or_else(|| rules::unknown_kind(kind))?;
    let mut payload = Map::new();
    for (field, required) in &fields {
        // An optional field (a model selection's prompt) is stored only when it is sent; a null
        // sends nothing.
        let value = match parameters.get(field) {
            Some(Value::Null) | None if !required => continue,
            value => value.ok_or_else(|| {
                Error::validation(format!(
                    "missing required parameter {field} for a {kind} component"
                ))
            })?,
        };
        payload.insert(field.clone(), canonical(value));
    }
    // A sampling kind's list starts empty and present, rather than absent until the first pick: a
    // payload whose shape depends on its history is a payload every reader has to special-case, and
    // an unsampled component is a real state — it selects nothing — not a missing one.
    if component_sample_limit(kind).is_some() {
        payload.insert(SAMPLES_FIELD.to_owned(), Value::Array(Vec::new()));
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
        .ok_or_else(|| Error::validation(format!("missing required parameter {name}")))
}

fn number(parameters: &Map<String, Value>, name: &str) -> Result<f64, Error> {
    parameters
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| Error::validation(format!("missing required parameter {name}")))
}

fn boolean(parameters: &Map<String, Value>, name: &str) -> Result<bool, Error> {
    parameters
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::validation(format!("missing required parameter {name}")))
}

fn mode(parameters: &Map<String, Value>) -> Result<ComponentMode, Error> {
    rules::mode(enumeration(parameters, "mode")?)
}

/// A destination index inside a list that currently holds `len` items. The declared range bounds the
/// request; this bounds it against the list it actually addresses, and names what it counted.
fn position(
    parameters: &Map<String, Value>,
    name: &str,
    len: usize,
    what: &str,
) -> Result<usize, Error> {
    rules::position(requested_index(parameters, name)?, len, what)
}

/// A checked index parameter as the request sent it, before it is bounded by any list.
fn requested_index(parameters: &Map<String, Value>, name: &str) -> Result<u64, Error> {
    parameters
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::validation(format!("missing required parameter {name}")))
}
