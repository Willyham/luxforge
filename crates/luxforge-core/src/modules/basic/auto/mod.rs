//! Basic's Auto tone: the analysis action and query, planned on the owner from metadata and
//! answered on the tile service's worker; the solver ([`solve`]) and the forward model it solves
//! through ([`forward_model`]). The sample grid it reads belongs to the tile layer
//! ([`crate::tiles::read_grid`]); nothing of Auto is in the grid's identity.
mod forward;
mod solve;

pub use forward::{ForwardModel, forward_model};
pub use solve::*;

use super::{BASIC_EFFECT, BasicModule, SET_BASIC};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, AnalysisAction, CompileStage, Error, StageContext,
    ToolModule,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub(super) const ID: &str = "auto-tone";

pub(super) fn descriptor() -> ActionDescriptor {
    let mut descriptor = ActionDescriptor::new(
        ID,
        "Auto tone",
        "Sets Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation absolutely on the global Basic layer in one undoable entry. Leaves white balance and other modules unchanged; repeats are no-ops. Analysis runs off the catalog owner. Refuses mask targets, historical selection, drafts and images without usable tonal range.",
    );
    // Lightroom's Auto chord.
    descriptor.shortcut = Some(crate::Chord::command('U'));
    descriptor.analysis = Some(AnalysisAction {
        query: ID.into(),
        writes: BTreeMap::from([(SET_BASIC.into(), FIELDS.map(str::to_owned).into())]),
    });
    descriptor
}

fn global(context: &StageContext<'_>) -> Result<(), Error> {
    if context.target.is_some() {
        return Err(Error::validation(
            "Auto tone applies to the photo's global Basic layer",
        ));
    }
    Ok(())
}

pub(super) fn plan(input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
    if input.action_id != ID {
        return Err(Error::validation(format!(
            "unknown action {}",
            input.action_id
        )));
    }
    global(context)?;
    let answer = context.questions.query(ID, &Map::new())?;
    let values: AutoToneValues = serde_json::from_value(answer["values"].clone())
        .map_err(|e| Error::internal(e.to_string()))?;
    BasicModule::new().plan(
        &ActionInput {
            action_id: SET_BASIC.into(),
            parameters: values.fields(),
        },
        context,
    )
}

pub(super) fn query(context: &StageContext<'_>) -> Result<Value, Error> {
    global(context)?;
    let index = context.own_layer(BASIC_EFFECT)?.map_or_else(
        || context.insertion_index_for(BASIC_EFFECT),
        |(index, _)| index,
    );
    let stage = CompileStage::exact(context.stage_before(index)?);
    let input = context.questions.grid_before(index)?;
    // Each evaluation retains one f64 luminance per point and one small RGB chunk. The grid is
    // accounted by its read and retained under the separate 32 MiB sample cap.
    let _scratch = input
        .context
        .scratch()
        .reserve(input.sample.rgb.len() * size_of::<f64>() + 4096 * 12);
    let model = forward_model(context.registry, context.layers, index, stage)?;
    let report = model.solve(&input.sample, AutoToneTargets::default(), &input.cancel)?;
    let values = report.values;
    let summary = format!(
        "Auto tone · {}\nExposure {:+.2} EV · Contrast {:+.0}\nHighlights {:+.0} · Shadows {:+.0}\nWhites {:+.0} · Blacks {:+.0}\nVibrance {:+.0} · Saturation {:+.0}\n{} usable samples ({} × {}); {} source-clipped, {} non-finite\nOutput median {:.3}; highlights {:.3}%, shadows {:.3}%\nBounds: {}",
        report.algorithm,
        values.exposure,
        values.contrast,
        values.highlights,
        values.shadows,
        values.whites,
        values.blacks,
        values.vibrance,
        values.saturation,
        report.sample.usable,
        report.sample.grid[0],
        report.sample.grid[1],
        report.sample.source_clipped,
        report.sample.non_finite,
        report.output.p50,
        report.output.highlight_clip * 100.,
        report.output.shadow_clip * 100.,
        if report.bounded.is_empty() {
            "none".into()
        } else {
            report.bounded.join(", ")
        }
    );
    let mut answer = serde_json::to_value(report).map_err(|e| Error::internal(e.to_string()))?;
    answer["summary"] = json!(summary);
    answer["forward_model"] = model.explanation();
    answer["renderer"] = serde_json::to_value(crate::Renderer::from(&input.answered))
        .map_err(|e| Error::internal(e.to_string()))?;
    answer["source_clip_detection"] =
        serde_json::to_value(input.clipping).map_err(|e| Error::internal(e.to_string()))?;
    if serde_json::to_vec(&answer)
        .map_err(|e| Error::internal(e.to_string()))?
        .len()
        > 4096
    {
        return Err(Error::resource_limit("Auto tone explanation exceeds 4 KiB"));
    }
    Ok(answer)
}
