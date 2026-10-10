//! The Corrections queries: the layer as stored, and the ranked automatic sources
//! (`docs/design/corrections.md`, "Queries").
use super::{
    CORRECTIONS_EFFECT,
    actions::{self, addressed, stroke_of},
    geometry,
    module::{MAX_SOURCES, OPERATION},
    payload::{MAX_CORRECTIONS, RepairPayload},
    search,
    stroke::CorrectionStroke,
};
use crate::{
    Error, StageContext,
    path::steps_to_units,
    tiles::region::{RegionRequest, RegionStage},
};
use serde_json::{Map, Value, json};

/// The repair layer the context's stack holds, read, with its index; or the index a new one would
/// take and the empty layer.
fn layer(context: &StageContext<'_>) -> Result<(usize, bool, RepairPayload), Error> {
    Ok(match context.own_layer(CORRECTIONS_EFFECT)? {
        Some((index, layer)) => (
            index,
            true,
            super::module::stored(&layer.effect_id, layer.effect_format, &layer.payload)?,
        ),
        None => (
            context.insertion_index_for(CORRECTIONS_EFFECT),
            false,
            RepairPayload::default(),
        ),
    })
}

/// `query.corrections`: the layer as stored, its operations in application order with their
/// hydrated paths. Reads the payload and its strokes and no pixel; a missing or corrupt stroke is
/// refused naming its operation.
pub(super) fn corrections(context: &StageContext<'_>) -> Result<Value, Error> {
    let (_, _, payload) = layer(context)?;
    let strokes = context.strokes()?;
    let operations = payload
        .operations
        .iter()
        .enumerate()
        .map(|(index, operation)| actions::listed(index, operation, strokes))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "limit": MAX_CORRECTIONS,
        "next": payload.next,
        "operations": operations,
    }))
}

/// `query.correction-sources`: the automatic sources of one correction, or of a new top one on a
/// path, ranked best first, without writing anything.
///
/// The input is the repair stage before the operation — `Pᵢ`, the layer's output with only the
/// operations below it, for an operation; the layer's whole output `Pₙ` for a path — read as one
/// bounded region at the search's reduction around the destination
/// ([`crate::StageQuestions::region_before`]), so the read and the search run off the catalog
/// owner. The ranking is the one `add-correction` stores rank 1 of.
pub(super) fn sources(
    parameters: &Map<String, Value>,
    context: &StageContext<'_>,
) -> Result<Value, Error> {
    let count = parameters
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_SOURCES as u64) as usize;
    let (index, held, payload) = layer(context)?;
    let path = parameters.get("points");
    let radius = parameters.get("radius").and_then(Value::as_f64);
    let (stroke, stage) = match (parameters.contains_key(OPERATION), path, radius) {
        (true, None, None) => {
            let (position, operation) = payload.find(addressed(parameters)?)?;
            let stroke = stroke_of(context.strokes()?, operation)?.clone();
            let stage = if position == 0 {
                RegionStage::Before(index)
            } else {
                RegionStage::OutputWith {
                    layer: index,
                    payload: RepairPayload {
                        operations: payload.operations[..position].to_vec(),
                        ..payload.clone()
                    }
                    .stored(),
                }
            };
            (stroke, stage)
        }
        (false, Some(path), Some(radius)) => {
            let stroke = CorrectionStroke::capture(&actions::points(path)?, radius)?;
            let stage = if held && !payload.operations.is_empty() {
                RegionStage::Before(index + 1)
            } else {
                RegionStage::Before(index)
            };
            (stroke, stage)
        }
        _ => {
            return Err(Error::validation(
                "send operation, or points with radius, to rank sources",
            ));
        }
    };
    let content = context.stage_before(index)?;
    if !geometry::meets_frame(geometry::footprint(&stroke, content)) {
        return Err(Error::validation(
            "a correction's destination must meet the photograph's frame",
        ));
    }
    let planned = search::plan(&stroke, content);
    // A destination no candidate survives for is no-source before any pixel is read.
    let ranked = if planned.region.width == 0 || planned.region.height == 0 {
        Vec::new()
    } else {
        let read = context.questions.region_before(&RegionRequest {
            stage,
            rect: planned.region,
            reduction: planned.r,
        })?;
        search::rank(&planned, &read)?
    };
    let anchor = stroke.anchor();
    let candidates: Vec<Value> = ranked
        .iter()
        .take(count)
        .enumerate()
        .map(|(rank, candidate)| {
            let source = [
                anchor[0] + candidate.offset[0],
                anchor[1] + candidate.offset[1],
            ];
            json!({
                "rank": rank + 1,
                "source-x": steps_to_units(source[0]),
                "source-y": steps_to_units(source[1]),
                "score": candidate.score,
            })
        })
        .collect();
    Ok(json!({
        "outcome": if candidates.is_empty() { "no-source" } else { "found" },
        "candidates": candidates,
    }))
}
