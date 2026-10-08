//! The model Auto solves through, built once from the stack: the global Basic layer at candidate
//! values, then the global Look layers after it, compiled by their own modules.
use super::{
    AutoToneReport, AutoToneTargets, AutoToneValues, ExposureSearch, solve_with_exposure_search,
};
use crate::{
    BASIC_EFFECT, Cancel, ColorOperation, CompileStage, Error, LOOK_EFFECT, Layer, ModuleRegistry,
    Processing, tiles::SampleGrid,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// What Auto evaluates a candidate through: Basic's stored payload, whose white balance it keeps,
/// with the eight Auto fields replaced, then the compiled units of every global Look layer after
/// Basic, then the output boundary's clamp. A Look before Basic is part of the analysed input, not
/// of this model. The Exposure search is monotone only when every later layer's module declares
/// its luminance response monotonic for its payload ([`crate::ToolModule::monotonic_luminance`]).
pub struct ForwardModel<'a> {
    registry: &'a ModuleRegistry,
    stage: CompileStage,
    basic: Map<String, Value>,
    downstream: Vec<ColorOperation>,
    search: ExposureSearch,
    used: Vec<&'static str>,
    omitted: BTreeMap<(String, bool), usize>,
}

/// The forward model of the global Basic layer at `index` in `layers`, or of the one a commit
/// would insert there when `layers[index]` is not it, compiled at `stage`, the stage Basic
/// receives. Reads payloads and compiles pointwise units only; renders nothing.
pub fn forward_model<'a>(
    registry: &'a ModuleRegistry,
    layers: &[Layer],
    index: usize,
    stage: CompileStage,
) -> Result<ForwardModel<'a>, Error> {
    let global = |layer: &Layer, effect: &str| layer.effect_id == effect && layer.mask.is_none();
    let (basic, after) = match layers.get(index) {
        Some(layer) if global(layer, BASIC_EFFECT) => (
            layer.payload.as_object().cloned().unwrap_or_default(),
            index + 1,
        ),
        _ => (Map::new(), index),
    };
    let later = layers.get(after..).unwrap_or_default();
    let mut downstream = Vec::new();
    let mut search = ExposureSearch::Monotone;
    let mut omitted: BTreeMap<(String, bool), usize> = BTreeMap::new();
    for layer in later {
        if !global(layer, LOOK_EFFECT) {
            *omitted
                .entry((layer.effect_id.clone(), layer.mask.is_some()))
                .or_default() += 1;
            continue;
        }
        let (module, _) = registry
            .effect(LOOK_EFFECT)
            .ok_or_else(|| Error::incompatible("Auto tone's Look provider is unavailable"))?;
        if !module.monotonic_luminance(LOOK_EFFECT, layer.effect_format, &layer.payload)? {
            search = ExposureSearch::Exhaustive;
        }
        let Processing::Color(unit) =
            module.compile(LOOK_EFFECT, layer.effect_format, &layer.payload, stage)?
        else {
            return Err(Error::internal("the Look did not compile to colour"));
        };
        downstream.push(unit);
    }
    let used = if downstream.is_empty() {
        vec![BASIC_EFFECT]
    } else {
        vec![BASIC_EFFECT, LOOK_EFFECT]
    };
    Ok(ForwardModel {
        registry,
        stage,
        basic,
        downstream,
        search,
        used,
        omitted,
    })
}

impl ForwardModel<'_> {
    /// The Exposure search this model permits.
    pub fn search(&self) -> ExposureSearch {
        self.search
    }

    /// The units a candidate is evaluated through, in order: Basic at `values`, then the Looks.
    pub fn compile(&self, values: AutoToneValues) -> Result<Vec<ColorOperation>, Error> {
        let (module, _) = self
            .registry
            .effect(BASIC_EFFECT)
            .ok_or_else(|| Error::incompatible("Auto tone's Basic provider is unavailable"))?;
        let mut payload = self.basic.clone();
        payload.extend(values.fields());
        let Processing::Color(basic) = module.compile(
            BASIC_EFFECT,
            crate::EFFECT_FORMAT,
            &Value::Object(payload),
            self.stage,
        )?
        else {
            return Err(Error::internal("Basic did not compile to colour"));
        };
        let mut units = Vec::with_capacity(1 + self.downstream.len());
        units.push(basic);
        units.extend(self.downstream.iter().cloned());
        Ok(units)
    }

    /// Solve `sample` through this model with its Exposure search.
    pub fn solve(
        &self,
        sample: &SampleGrid,
        targets: AutoToneTargets,
        cancel: &Cancel,
    ) -> Result<AutoToneReport, Error> {
        solve_with_exposure_search(
            sample,
            targets,
            |values| self.compile(values),
            self.search,
            cancel,
        )
    }

    /// The explanation's account of the model: the effects it used, and every later layer it left
    /// out, by effect and whether it is masked.
    pub fn explanation(&self) -> Value {
        json!({
            "used": self.used,
            "omitted": self.omitted.iter().map(|((effect, masked), count)| json!({"effect": effect, "masked": masked, "count": count})).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests;
