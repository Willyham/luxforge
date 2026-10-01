//! Optional offline distortion correction. A selection freezes profile terms and source eligibility;
//! every later evaluation reads only the layer payload.
pub(crate) mod index;
pub(crate) mod payload;
pub(crate) mod pinned;
pub(crate) mod resolve;

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Control, EffectDescriptor, EffectStage,
    ExactGeometry, LayerReport, LayerUpdate, ModuleDescriptor, NewLayer, ParameterDescriptor,
    Processing, QueryChoiceControl, ResetAction, Stage, StageContext, ToolModule, WarpStep,
};
use crate::{EFFECT_FORMAT, Error, Layer, SourceOptics};
use serde_json::{Map, Value, json};

pub const LENS_EFFECT: &str = "luxforge.lens.distortion";
const SELECT: &str = "select-lens-profile";
const RESET: &str = "reset-lens-profile";
const QUERY: &str = "lens-profiles";
fn acknowledgement_parameter() -> ParameterDescriptor {
    ParameterDescriptor::boolean("assume-uncorrected")
        .default(false)
        .notes("acknowledge that this selection assumes no existing distortion correction")
}
fn focal_parameter() -> ParameterDescriptor {
    ParameterDescriptor::number("focal", 0.5, 2000.0)
        .unit("mm")
        .notes("a focal length supplied explicitly when EXIF has none")
}
fn payload(effect: &str, format: u32, value: &Value) -> Result<payload::Payload, Error> {
    if effect != LENS_EFFECT {
        return Err(Error::unavailable_effect(effect, &[]));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::incompatible("Unsupported Lens correction format"));
    }
    payload::parse(value)
}
#[derive(Debug)]
pub(crate) struct LensModule {
    descriptor: ModuleDescriptor,
}
impl LensModule {
    pub(crate) fn new() -> Self {
        index::load_in_background();
        Self {
            descriptor: ModuleDescriptor {
                id: "luxforge.lens".into(),
                title: "Lens correction".into(),
                hint: Some("Profile distortion correction".into()),
                collapsed: true,
                effects: vec![EffectDescriptor {
                    order: 2,
                    single: true,
                    ..EffectDescriptor::new(LENS_EFFECT, EffectStage::Geometry)
                }],
                actions: vec![
                    ActionDescriptor {
                        preset: false,
                        parameters: vec![
                            ParameterDescriptor::string("profile", 32).required(true),
                            focal_parameter(),
                            acknowledgement_parameter(),
                        ],
                        ..ActionDescriptor::new(
                            SELECT,
                            "Select lens profile",
                            "resolves an offline profile against this photo and freezes its terms; never applies a match automatically",
                        )
                    },
                    ActionDescriptor {
                        preset: false,
                        ..ActionDescriptor::new(
                            RESET,
                            "Reset Lens correction",
                            "clears the selected profile while keeping the lens layer identity",
                        )
                    },
                ],
                queries: vec![ActionDescriptor {
                    preset: false,
                    parameters: vec![
                        ParameterDescriptor::string("text", 64).default(""),
                        ParameterDescriptor::integer("page", 0, 99).default(0),
                        focal_parameter(),
                        acknowledgement_parameter(),
                    ],
                    ..ActionDescriptor::new(
                        QUERY,
                        "Lens profiles",
                        "matching candidates first, then searchable offline profiles; each row reports eligibility and reasons, at most 50 per page",
                    )
                }],
                controls: vec![Control::QueryChoice(QueryChoiceControl {
                    label: "Profile".into(),
                    query: QUERY.into(),
                    text: "text".into(),
                    page: "page".into(),
                    action: SELECT.into(),
                    key: "profile".into(),
                    shared: vec!["focal".into(), "assume-uncorrected".into()],
                })],
                reset: Some(ResetAction {
                    action: RESET.into(),
                    preset: Map::new(),
                }),
                ..ModuleDescriptor::default()
            },
        }
    }
    fn input(
        &self,
        context: &StageContext<'_>,
        parameters: &Map<String, Value>,
    ) -> Result<(SourceOptics, resolve::ResolveInput), Error> {
        let optics = context.optics()?;
        let index = context
            .own_layer(LENS_EFFECT)?
            .map_or_else(|| context.insertion_index_for(LENS_EFFECT), |(i, _)| i);
        let stage = context.stage_before(index)?;
        let identity = &optics.identity;
        let input = resolve::ResolveInput {
            make: identity.make.clone().unwrap_or_default(),
            model: identity.model.clone().unwrap_or_default(),
            lens_model: identity.lens_model.clone(),
            focal_mm: identity.focal_mm,
            focal_35mm: identity.focal_35mm,
            focal_override: parameters.get("focal").and_then(Value::as_f64),
            stage,
        };
        Ok((optics, input))
    }
}
impl ToolModule for LensModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if self.descriptor.action(action_id).is_none() {
            return Err(Error::validation(format!("unknown action {action_id}")));
        }
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let existing = context.own_layer(LENS_EFFECT)?.map(|(_, layer)| layer);
        let new = match input.action_id.as_str() {
            RESET => {
                if existing.is_none()
                    || existing.is_some_and(|l| {
                        payload(&l.effect_id, l.effect_format, &l.payload)
                            .is_ok_and(|p| p.profile.is_none())
                    })
                {
                    return Ok(ActionPlan::NoOp);
                }
                json!({"profile":null})
            }
            SELECT => {
                let index = index::shared()?;
                let (optics, request) = self.input(context, &input.parameters)?;
                let key = input
                    .parameters
                    .get("profile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::validation("profile is required"))?;
                let resolution = resolve::resolve(&index, key, &request)?;
                let profile = payload::Profile::from_resolution(
                    resolution,
                    &optics,
                    input
                        .parameters
                        .get("assume-uncorrected")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )?;
                serde_json::to_value(payload::Payload {
                    profile: Some(profile),
                })
                .map_err(|e| Error::internal(e.to_string()))?
            }
            action => return Err(Error::validation(format!("unknown action {action}"))),
        };
        if let Some(layer) = existing {
            if layer.payload == new {
                Ok(ActionPlan::NoOp)
            } else {
                Ok(ActionPlan::Update(LayerUpdate::new(layer.id.clone(), new)))
            }
        } else {
            Ok(ActionPlan::Commit(NewLayer::new(LENS_EFFECT, new)))
        }
    }
    fn validate_payload(&self, effect: &str, format: u32, value: &Value) -> Result<(), Error> {
        payload(effect, format, value).map(|_| ())
    }
    fn describe(&self, effect: &str, format: u32, value: &Value) -> Result<LayerReport, Error> {
        match payload(effect, format, value)?.profile {
            None => Ok(LayerReport {
                summary: "Off".into(),
                neutral: true,
                ..LayerReport::default()
            }),
            Some(profile) => {
                let mut values = Map::from_iter([
                    ("profile".into(), json!(profile.key)),
                    (
                        "assume-uncorrected".into(),
                        json!(profile.optics.acknowledged.is_some()),
                    ),
                ]);
                if profile.focal.source == payload::FocalSource::Override {
                    values.insert("focal".into(), json!(profile.focal.mm));
                }
                Ok(LayerReport {
                    summary: format!("{} at {} mm", profile.lens.model, profile.focal.mm),
                    values,
                    neutral: false,
                })
            }
        }
    }
    fn planned_label(&self, input: &ActionInput, layers: &[Layer], fallback: &str) -> String {
        if input.action_id == RESET {
            return "Reset Lens correction".into();
        }
        layers
            .iter()
            .find(|layer| layer.effect_id == LENS_EFFECT)
            .and_then(|l| payload(&l.effect_id, l.effect_format, &l.payload).ok())
            .and_then(|p| p.profile)
            .map(|p| {
                if p.camera.fixed_mount {
                    format!("Lens profile {}", p.lens.model)
                } else {
                    format!("Lens profile {} at {} mm", p.lens.model, p.focal.mm)
                }
            })
            .unwrap_or_else(|| fallback.into())
    }
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if query_id != QUERY {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        let index = index::shared()?;
        let (optics, input) = self.input(context, parameters)?;
        let mut page = resolve::search(
            &index,
            &input,
            parameters.get("text").and_then(Value::as_str).unwrap_or(""),
            parameters.get("page").and_then(Value::as_u64).unwrap_or(0) as usize,
        );
        let camera = resolve::camera_match(&index, &input);
        let mut reasons = resolve::status_reasons(&index, &input);
        let assume = parameters
            .get("assume-uncorrected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let optics_reason = match optics.ledger.distortion.status {
            luxforge_raw::OpticalStatus::Applied => {
                Some(resolve::Reason::EmbeddedDistortionApplied)
            }
            luxforge_raw::OpticalStatus::Unknown if !assume => {
                Some(resolve::Reason::AssumeUncorrectedRequired)
            }
            _ => None,
        };
        if let Some(reason) = optics_reason {
            reasons.push(reason);
            for row in &mut page.rows {
                row.eligible = false;
                row.reasons.push(reason);
            }
        }
        let mut visible_shared = Vec::new();
        if input.focal_override.is_some()
            || input.focal_mm.is_none()
            || reasons.contains(&resolve::Reason::FocalOutOfRange)
        {
            visible_shared.push("focal");
        }
        if optics.ledger.distortion.status == luxforge_raw::OpticalStatus::Unknown {
            visible_shared.push("assume-uncorrected");
        }
        let summary = format!(
            "{} · {} · {} · distortion {} · lateral CA {} · shading {}",
            camera.map_or_else(
                || "Camera not in database".to_owned(),
                |c| format!("{} {}", c.maker, c.model)
            ),
            input.lens_model.as_deref().unwrap_or("Fixed camera lens"),
            input.focal_override.or(input.focal_mm).map_or_else(
                || "Focal length missing".to_owned(),
                |f| format!(
                    "{f} mm ({})",
                    if input.focal_override.is_some() {
                        "override"
                    } else {
                        "EXIF"
                    }
                )
            ),
            serde_json::to_value(optics.ledger.distortion.status)
                .unwrap()
                .as_str()
                .unwrap(),
            serde_json::to_value(optics.ledger.lateral_ca.status)
                .unwrap()
                .as_str()
                .unwrap(),
            serde_json::to_value(optics.ledger.shading.status)
                .unwrap()
                .as_str()
                .unwrap()
        );
        Ok(
            json!({"status":{"summary":summary,"visible_shared":visible_shared,"camera":camera.map(|c|json!({"maker":c.maker,"model":c.model})),"lens_model":input.lens_model,"focal_mm":input.focal_override.or(input.focal_mm),"focal_source":if input.focal_override.is_some(){Some("override")}else if input.focal_mm.is_some(){Some("exif")}else{None},"crop_factor":camera.map(|c|c.crop_factor),"distortion":optics.ledger.distortion,"lateral_ca":optics.ledger.lateral_ca,"shading":optics.ledger.shading,"reasons":reasons},"rows":page.rows,"page":page.page,"pages":page.pages,"total":page.total}),
        )
    }
    fn compile(
        &self,
        effect: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        match payload(effect, format, value)?.profile {
            None => Ok(Processing::ExactGeometry(ExactGeometry::identity(
                stage.width,
                stage.height,
            ))),
            Some(profile) => Ok(Processing::Warp(WarpStep::radial(
                match profile.model {
                    index::DistortionModel::Poly3 => super::RadialModel::Poly3,
                    index::DistortionModel::Poly5 => super::RadialModel::Poly5,
                    index::DistortionModel::PtLens => super::RadialModel::PtLens,
                },
                profile.terms,
                profile.normalization.unit_scale,
                stage,
            )?)),
        }
    }
}
#[cfg(test)]
mod tests;
