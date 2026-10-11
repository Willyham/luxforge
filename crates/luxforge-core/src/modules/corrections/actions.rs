//! The Corrections actions: parsing, planning against the repair layer, and the `correction` each
//! add and update answers (`docs/design/corrections.md`, "Actions").
use super::{
    CORRECTIONS_EFFECT, geometry,
    input::{self, RepairInput},
    module::{ACCEPT, ADD, DELETE, OPERATION, REORDER, RESET, SOURCES, UPDATE, repair_layer},
    payload::{
        self, Brush, MAX_CORRECTIONS, MAX_ID, Operation, OperationKind, Origin, Patch, PatchKind,
        RepairPayload,
    },
    stroke::CorrectionStroke,
};
use crate::{
    ActionInput, ActionPlan, Error, Layer, LayerUpdate, NewLayer, StageContext,
    path::{StrokeKind, StrokeTable, steps_to_units},
};
use serde_json::{Map, Value, json};

/// Normalize a checked request: the action's own pairing rules, which no declared range states —
/// a source is both coordinates or neither, and `source-origin` comes only with one.
pub(super) fn parse(
    action_id: &str,
    parameters: &Map<String, Value>,
) -> Result<ActionInput, Error> {
    match action_id {
        ADD | UPDATE => {
            let sent = |name: &str| parameters.contains_key(name);
            if sent("source-x") != sent("source-y") {
                return Err(Error::validation(
                    "send source-x and source-y together, or neither",
                ));
            }
            if sent("source-origin") && !sent("source-x") {
                return Err(Error::validation(
                    "source-origin applies only with source-x and source-y",
                ));
            }
        }
        DELETE | REORDER | RESET => {}
        ACCEPT => {
            if parameters
                .get("candidate")
                .and_then(Value::as_u64)
                .is_none_or(|candidate| candidate > 3)
            {
                return Err(Error::validation("candidate is a number from 0 to 3"));
            }
        }
        _ => return Err(Error::validation(format!("unknown action {action_id}"))),
    }
    Ok(ActionInput {
        action_id: action_id.to_owned(),
        parameters: parameters.clone(),
    })
}

/// A request's posted path.
pub(super) fn points(value: &Value) -> Result<Vec<[f64; 2]>, Error> {
    serde_json::from_value(value.clone())
        .map_err(|error| Error::validation(format!("invalid points: {error}")))
}

fn number(parameters: &Map<String, Value>, name: &str) -> Option<f64> {
    parameters.get(name).and_then(Value::as_f64)
}

fn integer(parameters: &Map<String, Value>, name: &str) -> Option<u8> {
    parameters
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
}

fn source(parameters: &Map<String, Value>) -> Option<[f64; 2]> {
    Some([
        number(parameters, "source-x")?,
        number(parameters, "source-y")?,
    ])
}

fn kind(parameters: &Map<String, Value>) -> Result<Option<OperationKind>, Error> {
    parameters
        .get("mode")
        .map(|mode| {
            mode.as_str()
                .and_then(OperationKind::parse)
                .ok_or_else(|| Error::validation("mode must be heal or clone"))
        })
        .transpose()
}

/// The operation number a request's `operation` names.
pub(super) fn addressed(parameters: &Map<String, Value>) -> Result<u32, Error> {
    let identity = parameters
        .get(OPERATION)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::validation("operation is required"))?;
    payload::number(identity)
}

/// The stroke `operation` references, resolved as a correction stroke: a missing, corrupt or
/// wrong-type stroke is refused naming the operation, never read as an empty repair.
pub(super) fn stroke_of<'t>(
    strokes: &'t StrokeTable,
    operation: &Brush,
) -> Result<&'t CorrectionStroke, Error> {
    strokes
        .resolve::<CorrectionStroke>(&operation.stroke)
        .map_err(|error| {
            Error::new(
                error.kind,
                format!(
                    "{} referenced by operation {} of Corrections",
                    error.detail,
                    operation.identity()
                ),
            )
        })
}

/// What one action does to the repair layer.
///
/// Every plan reads the layer and, for geometry, the content stage it receives — one prefix
/// compile, `O(layers)` — and nothing else, except an add without a source, which asks the
/// automatic source query through the deferred-query seam ([`crate::StageQuestions::query`]):
/// answered off the catalog owner, then replayed here. A draft's preview of such an add plans no
/// change, so a preview never reads.
pub(super) fn plan(input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
    let own = context.own_layer(CORRECTIONS_EFFECT)?;
    let (index, layer) = match own {
        Some((index, layer)) => (index, Some(layer)),
        None => (context.insertion_index_for(CORRECTIONS_EFFECT), None),
    };
    let current = match layer {
        Some(layer) => {
            super::module::stored(&layer.effect_id, layer.effect_format, &layer.payload)?
        }
        None => RepairPayload::default(),
    };
    let parameters = &input.parameters;
    let mut fresh = StrokeTable::default();
    let next = match input.action_id.as_str() {
        ADD => {
            let Some(next) = add(parameters, context, index, &current, &mut fresh)? else {
                return Ok(ActionPlan::NoOp);
            };
            next
        }
        UPDATE => {
            let Some(next) = update(parameters, context, index, &current, &mut fresh)? else {
                return Ok(ActionPlan::NoOp);
            };
            next
        }
        DELETE => {
            let (position, _) = current.find(addressed(parameters)?)?;
            let mut next = current.clone();
            next.operations.remove(position);
            next
        }
        REORDER => {
            let (position, _) = current.find(addressed(parameters)?)?;
            let to = parameters
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::validation("index is required"))?;
            let count = current.operations.len();
            let to = usize::try_from(to).unwrap_or(usize::MAX);
            if to >= count {
                return Err(Error::validation(format!(
                    "index {to} is outside the {count} corrections of this photograph"
                )));
            }
            if to == position {
                return Ok(ActionPlan::NoOp);
            }
            let mut next = current.clone();
            let moved = next.operations.remove(position);
            next.operations.insert(to, moved);
            next
        }
        RESET => {
            if current.operations.is_empty() {
                return Ok(ActionPlan::NoOp);
            }
            RepairPayload {
                operations: Vec::new(),
                ..current.clone()
            }
        }
        ACCEPT => accept(parameters, context, index, &current)?,
        other => return Err(Error::validation(format!("unknown action {other}"))),
    };
    // The layer lists exactly its patches' artifacts, which the host admits it by.
    let artifacts = next.artifacts();
    let payload = next.stored();
    Ok(match layer {
        Some(layer) => ActionPlan::Update(
            LayerUpdate::new(layer.id.clone(), payload)
                .with_strokes(fresh)
                .with_artifacts(artifacts),
        ),
        None => ActionPlan::Commit(
            NewLayer::new(CORRECTIONS_EFFECT, payload)
                .with_strokes(fresh)
                .with_artifacts(artifacts),
        ),
    })
}

/// The input identity a new top operation of `current`, the layer at `index`, would be generated
/// against now ([`input::repair_input`]).
pub(super) fn top_input(
    context: &StageContext<'_>,
    index: usize,
    current: &RepairPayload,
) -> Result<RepairInput, Error> {
    current_input(context, index, current, current.operations.len())
}

/// The input identity of the operation at `position` of `current`, the layer at `index`, as the
/// stack stands.
pub(super) fn current_input(
    context: &StageContext<'_>,
    index: usize,
    current: &RepairPayload,
    position: usize,
) -> Result<RepairInput, Error> {
    let source = context.questions.source_identity()?;
    let content = context.stage_before(index)?;
    input::repair_input(
        &source,
        content,
        context.layers,
        context.masks,
        index,
        &current.operations,
        position,
    )
}

/// Plan an accept: the candidate the catalog owner pinned and published, appended on top as a
/// `patch` operation, when it was generated against exactly what a new top operation reads now.
/// Only the owner's accept flow plans it, with the published candidate in hand
/// ([`crate::StageQuestions::accepting`]); a draft or any other caller is refused.
fn accept(
    parameters: &Map<String, Value>,
    context: &StageContext<'_>,
    index: usize,
    current: &RepairPayload,
) -> Result<RepairPayload, Error> {
    let Some(accepted) = context.questions.accepting() else {
        return Err(Error::validation(
            "accept-correction-patch accepts a published candidate and cannot be drafted",
        ));
    };
    if current.operations.len() >= MAX_CORRECTIONS {
        return Err(limit_refusal());
    }
    if current.next > MAX_ID {
        return Err(Error::resource_limit(
            "this photograph has used every correction number",
        ));
    }
    let now = top_input(context, index, current)?;
    let reasons = accepted.input.reasons(&now);
    if !reasons.is_empty() {
        let names: Vec<&str> = reasons.iter().map(|reason| reason.as_str()).collect();
        return Err(Error::conflict(format!(
            "the candidate was generated against another input: {}",
            reasons
                .iter()
                .map(|reason| reason.sentence())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .with_data(json!({"reasons": names})));
    }
    let [x, y, width, height] = accepted.rect;
    let content = context.stage_before(index)?;
    if u64::from(x) + u64::from(width) > u64::from(content.width)
        || u64::from(y) + u64::from(height) > u64::from(content.height)
    {
        return Err(Error::validation(format!(
            "the patch [{x}, {y}, {width}, {height}] leaves the {}x{} photograph",
            content.width, content.height
        )));
    }
    let id = u32::try_from(current.next).expect("next was checked against the last number");
    let mut next = current.clone();
    next.next += 1;
    next.operations.push(Operation::Patch(Patch {
        id,
        kind: PatchKind::Patch,
        label: accepted.label.clone(),
        artifact: accepted.artifact.clone(),
        rect: accepted.rect,
        opacity: integer(parameters, "opacity").unwrap_or(100),
        input: accepted.input.clone(),
        provenance: accepted.provenance.clone(),
    }));
    if next.artifacts().len() > crate::artifacts::MAX_LAYER_ARTIFACTS {
        return Err(Error::resource_limit(format!(
            "{} patches is this photograph's limit",
            crate::artifacts::MAX_LAYER_ARTIFACTS
        ))
        .with_data(json!({"limit": crate::artifacts::MAX_LAYER_ARTIFACTS})));
    }
    Ok(next)
}

/// The content stage the repair layer at `index` receives, which its geometry addresses.
fn content(context: &StageContext<'_>, index: usize) -> Result<crate::Stage, Error> {
    context.stage_before(index)
}

/// Plan an add: the new operation on top, or `None` for a draft's preview of an add whose source
/// is automatic, which changes nothing until it commits.
fn add(
    parameters: &Map<String, Value>,
    context: &StageContext<'_>,
    index: usize,
    current: &RepairPayload,
    fresh: &mut StrokeTable,
) -> Result<Option<RepairPayload>, Error> {
    if current.operations.len() >= MAX_CORRECTIONS {
        return Err(limit_refusal());
    }
    if current.next > MAX_ID {
        return Err(Error::resource_limit(
            "this photograph has used every correction number",
        ));
    }
    let path = points(
        parameters
            .get("points")
            .ok_or_else(|| Error::validation("points is required"))?,
    )?;
    let radius =
        number(parameters, "radius").ok_or_else(|| Error::validation("radius is required"))?;
    let stroke = CorrectionStroke::capture(&path, radius)?;
    let stage = content(context, index)?;
    let destination = geometry::footprint(&stroke, stage);
    if !geometry::meets_frame(destination) {
        return Err(Error::validation(
            "a correction's destination must meet the photograph's frame",
        ));
    }
    let (offset, origin) = match source(parameters) {
        Some(position) => (geometry::offset_to(&stroke, position)?, Origin::Set),
        None if context.previewing() => return Ok(None),
        None => (automatic(context, &stroke)?, Origin::Auto),
    };
    geometry::check_footprints(&stroke, offset, stage)?;
    let id = u32::try_from(current.next).expect("next was checked against the last number");
    let mut next = current.clone();
    next.next += 1;
    next.operations.push(Operation::Brush(Brush {
        id,
        kind: kind(parameters)?.unwrap_or(OperationKind::Heal),
        stroke: fresh.insert(stroke),
        feather: integer(parameters, "feather").unwrap_or(50),
        opacity: integer(parameters, "opacity").unwrap_or(100),
        offset,
        origin,
    }));
    Ok(Some(next))
}

/// The refusal of an add at the operation limit, in the words `draft.begin` refuses it with
/// ([`crate::operations_limit`]).
pub(crate) fn limit_refusal() -> Error {
    crate::operations_limit(crate::EffectOperations {
        identity: crate::IdentityKind::Correction,
        max: MAX_CORRECTIONS as u32,
    })
}

/// The automatic source of a new top operation drawn with `stroke`: rank 1 of
/// `query.correction-sources` for its path, asked through the deferred-query seam so the add and
/// the query rank identically by construction, stored as an exact offset. No candidate is the
/// explicit `no-source` refusal.
fn automatic(context: &StageContext<'_>, stroke: &CorrectionStroke) -> Result<[i32; 2], Error> {
    let mut parameters = Map::new();
    parameters.insert(
        "points".into(),
        Value::Array(stroke.points().map(|[x, y]| json!([x, y])).collect()),
    );
    parameters.insert("radius".into(), json!(stroke.radius()));
    parameters.insert("count".into(), json!(1));
    let answer = context.questions.query(SOURCES, &parameters)?;
    let best = answer
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first());
    let Some(best) = best else {
        return Err(no_source());
    };
    let position = [
        best.get("source-x").and_then(Value::as_f64),
        best.get("source-y").and_then(Value::as_f64),
    ];
    let [Some(x), Some(y)] = position else {
        return Err(Error::internal("an automatic source answered no position"));
    };
    geometry::offset_to(stroke, [x, y])
}

/// The explicit outcome of an add whose automatic search found nothing.
pub(crate) fn no_source() -> Error {
    Error::validation("No automatic source fits; send source-x and source-y")
        .with_data(json!({"reason": "no-source"}))
}

/// Plan an update: the operation with every sent field applied, or `None` when that is the
/// operation as stored — the same stroke address, offset, numbers, kind and origin.
fn update(
    parameters: &Map<String, Value>,
    context: &StageContext<'_>,
    index: usize,
    current: &RepairPayload,
    fresh: &mut StrokeTable,
) -> Result<Option<RepairPayload>, Error> {
    let (position, operation) = current.find(addressed(parameters)?)?;
    let operation = match operation {
        Operation::Brush(brush) => brush,
        Operation::Patch(patch) => return update_patch(parameters, current, position, patch),
    };
    let strokes = context.strokes()?;
    let stored = stroke_of(strokes, operation)?;
    let radius = number(parameters, "radius");
    let stroke = match (parameters.get("points"), radius) {
        // A new path is captured at the resulting radius.
        (Some(path), _) => {
            CorrectionStroke::capture(&points(path)?, radius.unwrap_or(stored.radius()))?
        }
        (None, Some(radius)) => stored.with_radius(radius)?,
        (None, None) => stored.clone(),
    };
    // A new anchor keeps the source where it was, unless a source is sent too.
    let (offset, origin) = match source(parameters) {
        Some(position) => (
            geometry::offset_to(&stroke, position)?,
            match parameters.get("source-origin").and_then(Value::as_str) {
                Some(origin) => Origin::parse(origin)
                    .ok_or_else(|| Error::validation("source-origin must be auto or set"))?,
                None => Origin::Set,
            },
        ),
        None => {
            let source = geometry::source_steps(stored, operation);
            let anchor = stroke.anchor();
            (
                geometry::checked_offset([source[0] - anchor[0], source[1] - anchor[1]])?,
                operation.origin,
            )
        }
    };
    let changed = Brush {
        id: operation.id,
        kind: kind(parameters)?.unwrap_or(operation.kind),
        stroke: stroke.id(),
        feather: integer(parameters, "feather").unwrap_or(operation.feather),
        opacity: integer(parameters, "opacity").unwrap_or(operation.opacity),
        offset,
        origin,
    };
    if changed == *operation {
        return Ok(None);
    }
    geometry::check_footprints(&stroke, offset, content(context, index)?)?;
    if changed.stroke != operation.stroke {
        fresh.insert(stroke);
    }
    let mut next = current.clone();
    next.operations[position] = Operation::Brush(changed);
    Ok(Some(next))
}

/// Plan an update of a frozen patch: only its opacity changes; a geometry, mode or source field is
/// refused, and the same opacity is `None`.
fn update_patch(
    parameters: &Map<String, Value>,
    current: &RepairPayload,
    position: usize,
    patch: &Patch,
) -> Result<Option<RepairPayload>, Error> {
    if let Some(field) = parameters
        .keys()
        .find(|field| !matches!(field.as_str(), OPERATION | "opacity"))
    {
        return Err(Error::validation(format!(
            "{} is a frozen patch; only its opacity changes, not {field}",
            Operation::Patch(patch.clone()).name()
        )));
    }
    let opacity = integer(parameters, "opacity").unwrap_or(patch.opacity);
    if opacity == patch.opacity {
        return Ok(None);
    }
    let mut next = current.clone();
    next.operations[position] = Operation::Patch(Patch {
        opacity,
        ..patch.clone()
    });
    Ok(Some(next))
}

/// The `correction` an add, an update or an accept answers, read from the stack the request left
/// current: the operation it added (the newest number) or addressed, its name, its index and, for a
/// Clone or Heal, its absolute source. Nothing for the other actions, and nothing when the
/// operation is not there.
pub(super) fn answer(
    input: &ActionInput,
    layers: &[Layer],
    strokes: &StrokeTable,
) -> Result<Map<String, Value>, Error> {
    let mut answer = Map::new();
    let Some((_, _, payload)) = repair_layer(layers)? else {
        return Ok(answer);
    };
    let id = match input.action_id.as_str() {
        ADD | ACCEPT => u32::try_from(payload.next - 1).ok(),
        UPDATE => addressed(&input.parameters).ok(),
        _ => None,
    };
    let Some((index, operation)) = id.and_then(|id| payload.find(id).ok()) else {
        return Ok(answer);
    };
    let mut correction = json!({
        "operation": operation.identity(),
        "name": operation.name(),
        "index": index,
    });
    if let Operation::Brush(brush) = operation {
        let stroke = stroke_of(strokes, brush)?;
        let [x, y] = geometry::source(stroke, brush);
        correction["source-x"] = json!(x);
        correction["source-y"] = json!(y);
        correction["source-origin"] = json!(brush.origin.as_str());
    }
    answer.insert("correction".into(), correction);
    Ok(answer)
}

/// One stored operation as `query.corrections` lists it: a Clone or Heal with its hydrated path and
/// absolute source; a patch with its noun, artifact, rectangle in content pixels and provenance.
/// `stale` is the patch's reasons when it is stale, and absent otherwise.
pub(super) fn listed(
    index: usize,
    operation: &Operation,
    strokes: &StrokeTable,
    stale: &[input::StaleReason],
) -> Result<Value, Error> {
    let brush = match operation {
        Operation::Brush(brush) => brush,
        Operation::Patch(patch) => {
            let mut row = json!({
                "operation": operation.identity(),
                "name": operation.name(),
                "kind": "patch",
                "index": index,
                "label": patch.label,
                "artifact": patch.artifact,
                "rect": patch.rect,
                "opacity": patch.opacity,
                "provenance": patch.provenance,
            });
            if !stale.is_empty() {
                row["stale"] = json!({"reasons": stale});
            }
            return Ok(row);
        }
    };
    let stroke = stroke_of(strokes, brush)?;
    let [x, y] = geometry::source(stroke, brush);
    Ok(json!({
        "operation": operation.identity(),
        "name": operation.name(),
        "kind": brush.kind.as_str(),
        "index": index,
        "points": stroke.points().map(|[x, y]| json!([x, y])).collect::<Vec<_>>(),
        "radius": steps_to_units(stroke.radius_steps()),
        "feather": brush.feather,
        "opacity": brush.opacity,
        "source-x": x,
        "source-y": y,
        "source-origin": brush.origin.as_str(),
    }))
}

/// Each stale patch of `payload`, the layer at `index` of the context's stack, with its reasons, in
/// operation order: what `query.corrections`, `recipe.describe` and export disclose. Computes one
/// input per patch, `O(operations)` serialization each; reads no pixel.
pub(crate) fn stale(
    context: &StageContext<'_>,
    index: usize,
    payload: &RepairPayload,
) -> Result<Vec<(usize, Vec<input::StaleReason>)>, Error> {
    let mut found = Vec::new();
    for (position, operation) in payload.operations.iter().enumerate() {
        let Operation::Patch(patch) = operation else {
            continue;
        };
        let now = current_input(context, index, payload, position)?;
        let reasons = patch.input.reasons(&now);
        if !reasons.is_empty() {
            found.push((position, reasons));
        }
    }
    Ok(found)
}
