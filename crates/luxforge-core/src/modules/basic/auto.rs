//! Basic's analysis action: metadata planning on the owner, actual compiled units on its worker.
use super::{BASIC_EFFECT, BasicModule, SET_BASIC};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, AnalysisAction, CompileStage, EFFECT_FORMAT, Error,
    LOOK_EFFECT, Processing, StageContext, ToolModule,
    auto_tone::{self, AutoToneTargets, AutoToneValues},
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
        writes: BTreeMap::from([(
            SET_BASIC.into(),
            auto_tone::FIELDS.map(str::to_owned).into(),
        )]),
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
    let existing = context.own_layer(BASIC_EFFECT)?;
    let index = existing.map_or_else(
        || context.insertion_index_for(BASIC_EFFECT),
        |(index, _)| index,
    );
    let stage = CompileStage::exact(context.stage_before(index)?);
    let input = context.questions.analysis_before(index)?;
    // Each evaluation retains one f64 luminance per point and one small RGB chunk. The grid is
    // accounted by its read and retained under the separate 32 MiB sample cap.
    let _scratch = input
        .context
        .scratch()
        .reserve(input.sample.rgb.len() * size_of::<f64>() + 4096 * 12);
    let payload = existing
        .and_then(|(_, layer)| layer.payload.as_object())
        .cloned()
        .unwrap_or_default();
    let basic = BasicModule::new();
    let look = context
        .layers
        .iter()
        .find(|layer| layer.effect_id == LOOK_EFFECT && layer.mask.is_none());
    let look_unit = look
        .map(|layer| {
            let (module, _) = context
                .registry
                .effect(LOOK_EFFECT)
                .ok_or_else(|| Error::incompatible("Auto tone's Look provider is unavailable"))?;
            module.compile(LOOK_EFFECT, layer.effect_format, &layer.payload, stage)
        })
        .transpose()?;
    let search = if look.is_some_and(|layer| {
        layer.payload["amount"]
            .as_f64()
            .is_some_and(|amount| amount > 100.)
    }) {
        auto_tone::ExposureSearch::Exhaustive
    } else {
        auto_tone::ExposureSearch::Monotone
    };
    let report = auto_tone::solve_with_exposure_search(
        &input.sample,
        AutoToneTargets::default(),
        |values| {
            let mut payload = payload.clone();
            payload.extend(values.fields());
            let Processing::Color(basic) =
                basic.compile(BASIC_EFFECT, EFFECT_FORMAT, &Value::Object(payload), stage)?
            else {
                return Err(Error::internal("Basic did not compile to colour"));
            };
            let mut units = vec![basic];
            if let Some(Processing::Color(look)) = &look_unit {
                units.push(look.clone());
            }
            Ok(units)
        },
        search,
        &input.cancel,
    )?;
    let values = report.values;
    let summary = format!(
        "Auto tone · {}\nExposure {:+.2} EV · Contrast {:+.0}\nHighlights {:+.0} · Shadows {:+.0}\nWhites {:+.0} · Blacks {:+.0}\nVibrance {:+.0} · Saturation {:+.0}\n{} usable samples ({} × {}); {} source-white, {} non-finite\nOutput median {:.3}; highlights {:.3}%, shadows {:.3}%\nBounds: {}",
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
        report.sample.source_white,
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
    let mut omitted: BTreeMap<(String, bool), usize> = BTreeMap::new();
    for layer in &context.layers[index..] {
        if layer.mask.is_none()
            && (layer.effect_id == BASIC_EFFECT || layer.effect_id == LOOK_EFFECT)
        {
            continue;
        }
        *omitted
            .entry((layer.effect_id.clone(), layer.mask.is_some()))
            .or_default() += 1;
    }
    answer["summary"] = json!(summary);
    answer["forward_model"] = json!({
        "used": if look.is_some() { vec![BASIC_EFFECT, LOOK_EFFECT] } else { vec![BASIC_EFFECT] },
        "omitted": omitted.into_iter().map(|((effect, masked), count)| json!({"effect":effect,"masked":masked,"count":count})).collect::<Vec<_>>()
    });
    answer["renderer"] = serde_json::to_value(crate::Renderer::from(&input.answered))
        .map_err(|e| Error::internal(e.to_string()))?;
    answer["source_white_detection"] = json!(match context.kind {
        crate::SourceTag::Jpeg => "jpeg-code-255",
        crate::SourceTag::Raw => "sensor-mask-unavailable",
    });
    if serde_json::to_vec(&answer)
        .map_err(|e| Error::internal(e.to_string()))?
        .len()
        > 4096
    {
        return Err(Error::resource_limit("Auto tone explanation exceeds 4 KiB"));
    }
    Ok(answer)
}
