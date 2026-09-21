//! The required source-stage interpretation of a RAW original.
pub mod neutral;
pub mod white_balance;
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    EffectDescriptor, EffectStage, ExactGeometry, ModuleDescriptor, ParameterDescriptor,
    ParameterKind, Processing, ResetAction, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId, RAW_EFFECT};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const SET_EXPOSURE: &str = "set-raw-exposure";
const SET_RED: &str = "set-raw-red-gain";
const SET_BLUE: &str = "set-raw-blue-gain";
const SET_TEMPERATURE: &str = "set-raw-temperature";
const SET_TINT: &str = "set-raw-tint";
const PICK_NEUTRAL: &str = "pick-raw-neutral";
const AS_SHOT: &str = "use-as-shot-wb";
const RESET: &str = "reset-raw";
pub const MAX_RAW_GAIN: f64 = 32.0;

fn validation(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhiteBalanceMode {
    AsShot,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPayload {
    pub exposure_ev: f64,
    pub wb_mode: WhiteBalanceMode,
    /// Green-normalized pre-demosaic sensor gains; unused while AsShot is selected.
    pub gains: [f32; 3],
    /// Immutable camera as-shot gains carried by the Original layer.
    pub as_shot_gains: [f32; 3],
    /// Capture calibration, not an editable white-balance setting.
    pub cam_xyz: [[f32; 3]; 4],
    /// Explicit custom controls; AsShot does not imply a measured Kelvin value.
    pub temperature_kelvin: Option<f64>,
    pub tint: Option<f64>,
}

impl Default for RawPayload {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            wb_mode: WhiteBalanceMode::AsShot,
            gains: [1.0; 3],
            as_shot_gains: [1.0; 3],
            cam_xyz: [[0.0; 3]; 4],
            temperature_kelvin: None,
            tint: None,
        }
    }
}

impl RawPayload {
    pub fn for_as_shot(gains: [f32; 3], cam_xyz: [[f32; 3]; 4]) -> Result<Self, Error> {
        let payload = Self {
            exposure_ev: 0.0,
            wb_mode: WhiteBalanceMode::AsShot,
            gains,
            as_shot_gains: gains,
            cam_xyz,
            temperature_kelvin: None,
            tint: None,
        };
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if !self.exposure_ev.is_finite() || !(-5.0..=5.0).contains(&self.exposure_ev) {
            return Err(validation("RAW exposure must be -5..=+5 EV"));
        }
        if self.gains[1] != 1.0
            || self.as_shot_gains[1] != 1.0
            || self
                .gains
                .iter()
                .chain(self.as_shot_gains.iter())
                .any(|g| !g.is_finite() || *g <= 0.0 || f64::from(*g) > MAX_RAW_GAIN)
        {
            return Err(validation(
                "RAW gains must be finite, positive, <=32, and green-normalized",
            ));
        }
        if self
            .cam_xyz
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(validation("RAW camera calibration must be finite"));
        }
        match (self.temperature_kelvin, self.tint) {
            (None, None) => {}
            (Some(temperature), Some(tint)) => {
                let resolved =
                    white_balance::gains_from_temperature_tint(temperature, tint, self.cam_xyz)?;
                if self
                    .gains
                    .iter()
                    .zip(resolved)
                    .any(|(actual, expected)| (actual - expected).abs() > 1e-5)
                {
                    return Err(validation(
                        "RAW custom temperature/tint disagree with sensor gains",
                    ));
                }
            }
            _ => return Err(validation("RAW custom temperature and tint must be paired")),
        }
        Ok(())
    }

    pub fn from_layer(layer: &Layer) -> Result<Self, Error> {
        if layer.effect_id != RAW_EFFECT || layer.effect_format != EFFECT_FORMAT {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "invalid RAW source layer",
            ));
        }
        let payload: Self = serde_json::from_value(layer.payload.clone())
            .map_err(|e| validation(format!("invalid RAW payload: {e}")))?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn layer(&self, id: LayerId) -> Layer {
        Layer {
            id,
            effect_id: RAW_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(self).expect("validated RAW payload serializes"),
        }
    }
}

fn number(name: &str, min: f64, max: f64, default: f64, unit: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number { min, max },
        required: true,
        default: Some(Value::from(default)),
        unit: Some(unit.into()),
        notes: "finite value".into(),
    }
}

fn coordinate(name: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Integer {
            min: 0,
            max: 16_383,
        },
        required: true,
        default: None,
        unit: Some("px".into()),
        notes: "upright RAW content coordinate".into(),
    }
}

fn action(id: &str, title: &str, parameters: Vec<ParameterDescriptor>) -> ActionDescriptor {
    ActionDescriptor {
        id: id.into(),
        title: title.into(),
        notes: "updates the required RAW source layer through ordinary history".into(),
        summary: None,
        parameters,
    }
}

#[derive(Debug)]
pub struct RawModule {
    descriptor: ModuleDescriptor,
}

impl Default for RawModule {
    fn default() -> Self {
        Self::new()
    }
}

impl RawModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.raw".into(),
                title: "RAW".into(),
                hint: Some("Source development".into()),
                effects: vec![EffectDescriptor {
                    id: RAW_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Source,
                }],
                actions: vec![
                    action(
                        SET_EXPOSURE,
                        "Exposure",
                        vec![number("ev", -5.0, 5.0, 0.0, "EV")],
                    ),
                    action(
                        SET_TEMPERATURE,
                        "Custom temperature",
                        vec![number("kelvin", 2000.0, 12000.0, 6504.0, "K")],
                    ),
                    action(
                        SET_TINT,
                        "Custom tint",
                        vec![number("tint", -100.0, 100.0, 0.0, "Lightwell")],
                    ),
                    action(
                        SET_RED,
                        "Red gain",
                        vec![number("gain", 0.01, MAX_RAW_GAIN, 1.0, "×")],
                    ),
                    action(
                        SET_BLUE,
                        "Blue gain",
                        vec![number("gain", 0.01, MAX_RAW_GAIN, 1.0, "×")],
                    ),
                    action(
                        PICK_NEUTRAL,
                        "Pick neutral patch",
                        vec![coordinate("x"), coordinate("y")],
                    ),
                    action(AS_SHOT, "As shot white balance", vec![]),
                    action(RESET, "Reset RAW", vec![]),
                ],
                controls: vec![Control::Group {
                    label: "RAW development".into(),
                    reset: Some(ResetAction {
                        action: RESET.into(),
                        preset: Map::new(),
                    }),
                    controls: vec![
                        Control::Number {
                            action: SET_EXPOSURE.into(),
                            parameter: "ev".into(),
                            label: "Exposure".into(),
                        },
                        Control::Number {
                            action: SET_TEMPERATURE.into(),
                            parameter: "kelvin".into(),
                            label: "Custom temperature".into(),
                        },
                        Control::Number {
                            action: SET_TINT.into(),
                            parameter: "tint".into(),
                            label: "Custom tint".into(),
                        },
                        Control::Action {
                            action: AS_SHOT.into(),
                            label: "As shot".into(),
                            preset: Map::new(),
                        },
                    ],
                }],
                reset: Some(ResetAction {
                    action: RESET.into(),
                    preset: Map::new(),
                }),
                canvas: Some(CanvasInteraction::PointPick {
                    action: PICK_NEUTRAL.into(),
                    x: "x".into(),
                    y: "y".into(),
                    title: "Neutral WB".into(),
                    shortcut: Some("W".into()),
                }),
                developer: false,
                availability: Availability::Available,
            },
        }
    }
}

impl ToolModule for RawModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if ![
            SET_EXPOSURE,
            SET_TEMPERATURE,
            SET_TINT,
            SET_RED,
            SET_BLUE,
            PICK_NEUTRAL,
            AS_SHOT,
            RESET,
        ]
        .contains(&action_id)
        {
            return Err(validation(format!("unknown RAW action {action_id}")));
        }
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }
    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let layer = stage
            .layers
            .first()
            .filter(|layer| layer.effect_id == RAW_EFFECT)
            .ok_or_else(|| validation("RAW controls require a RAW original"))?;
        let mut payload = RawPayload::from_layer(layer)?;
        match input.action_id.as_str() {
            SET_EXPOSURE => {
                payload.exposure_ev = input
                    .parameters
                    .get("ev")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| validation("missing EV"))?
            }
            SET_RED | SET_BLUE => {
                if payload.wb_mode == WhiteBalanceMode::AsShot {
                    payload.gains = payload.as_shot_gains;
                }
                let gain = input
                    .parameters
                    .get("gain")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| validation("missing gain"))? as f32;
                payload.gains[if input.action_id == SET_RED { 0 } else { 2 }] = gain;
                payload.wb_mode = WhiteBalanceMode::Custom;
                payload.temperature_kelvin = None;
                payload.tint = None;
            }
            SET_TEMPERATURE | SET_TINT => {
                let temperature = if input.action_id == SET_TEMPERATURE {
                    input
                        .parameters
                        .get("kelvin")
                        .and_then(Value::as_f64)
                        .ok_or_else(|| validation("missing Kelvin"))?
                } else {
                    payload.temperature_kelvin.unwrap_or(6504.0)
                };
                let tint = if input.action_id == SET_TINT {
                    input
                        .parameters
                        .get("tint")
                        .and_then(Value::as_f64)
                        .ok_or_else(|| validation("missing tint"))?
                } else {
                    payload.tint.unwrap_or(0.0)
                };
                payload.gains =
                    white_balance::gains_from_temperature_tint(temperature, tint, payload.cam_xyz)?;
                payload.temperature_kelvin = Some(temperature);
                payload.tint = Some(tint);
                payload.wb_mode = WhiteBalanceMode::Custom;
            }
            PICK_NEUTRAL => {
                let x = input
                    .parameters
                    .get("x")
                    .and_then(Value::as_u64)
                    .and_then(|x| u32::try_from(x).ok())
                    .ok_or_else(|| validation("missing neutral x"))?;
                let y = input
                    .parameters
                    .get("y")
                    .and_then(Value::as_u64)
                    .and_then(|y| u32::try_from(y).ok())
                    .ok_or_else(|| validation("missing neutral y"))?;
                payload.gains = stage
                    .sensor_neutral
                    .ok_or_else(|| validation("RAW sensor is unavailable"))?(
                    x, y
                )?;
                payload.temperature_kelvin = None;
                payload.tint = None;
                payload.wb_mode = WhiteBalanceMode::Custom;
            }
            AS_SHOT => payload.wb_mode = WhiteBalanceMode::AsShot,
            RESET => payload = RawPayload::for_as_shot(payload.as_shot_gains, payload.cam_xyz)?,
            _ => return Err(validation("unknown RAW action")),
        }
        payload.validate()?;
        if payload == RawPayload::from_layer(layer)? {
            return Ok(ActionPlan::NoOp);
        }
        Ok(ActionPlan::Update(payload.layer(layer.id.clone())))
    }
    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        RawPayload::from_layer(&Layer {
            id: LayerId::new(),
            effect_id: effect_id.into(),
            effect_format: format,
            payload: value.clone(),
        })
        .map(|_| ())
    }
    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let payload = RawPayload::from_layer(&Layer {
            id: LayerId::new(),
            effect_id: effect_id.into(),
            effect_format: format,
            payload: value.clone(),
        })?;
        Ok(format!(
            "Exposure {:+.2} EV · {} WB",
            payload.exposure_ev,
            match payload.wb_mode {
                WhiteBalanceMode::AsShot => "as-shot",
                WhiteBalanceMode::Custom => "custom",
            }
        ))
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        self.validate_payload(effect_id, format, value)?;
        Ok(Processing::ExactGeometry(ExactGeometry {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: 0,
            ty: 0,
            output_width: stage.width,
            output_height: stage.height,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn planned(layer: &Layer, action: &str, params: Value) -> Result<RawPayload, Error> {
        let module = RawModule::new();
        let unavailable = |_: u32, _: u32| Ok(None);
        let stage_before = |_: usize| {
            Ok(Stage {
                width: 32,
                height: 32,
            })
        };
        let insertion_index = |_: EffectStage| 0;
        let sample_before = |_: usize, _: u32, _: u32| Ok(None);
        let neutral = |_: u32, _: u32| Ok([1.4, 1.0, 1.6]);
        let context = StageContext {
            stage: Stage {
                width: 32,
                height: 32,
            },
            layers: std::slice::from_ref(layer),
            sampler: &unavailable,
            stage_before: &stage_before,
            insertion_index: &insertion_index,
            sample_before: &sample_before,
            sensor_neutral: Some(&neutral),
        };
        let input = module.parse(action, params.as_object().unwrap())?;
        match module.plan(&input, &context)? {
            ActionPlan::Update(next) => RawPayload::from_layer(&next),
            _ => Err(validation("expected RAW update")),
        }
    }

    #[test]
    fn temperature_tint_and_picker_share_one_sensor_gain_payload() {
        let matrix = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
        ];
        let original = RawPayload::for_as_shot([2.0, 1.0, 1.5], matrix).unwrap();
        let layer = original.layer(LayerId::new());
        let temperature = planned(&layer, SET_TEMPERATURE, json!({"kelvin":5500.0})).unwrap();
        assert_eq!(temperature.wb_mode, WhiteBalanceMode::Custom);
        assert_eq!(temperature.temperature_kelvin, Some(5500.0));
        assert_eq!(temperature.tint, Some(0.0));
        let tint = planned(
            &temperature.layer(layer.id.clone()),
            SET_TINT,
            json!({"tint":12.0}),
        )
        .unwrap();
        assert_eq!(tint.temperature_kelvin, Some(5500.0));
        assert_eq!(tint.tint, Some(12.0));
        assert_ne!(tint.gains, temperature.gains);
        let picked = planned(
            &tint.layer(layer.id.clone()),
            PICK_NEUTRAL,
            json!({"x":10,"y":11}),
        )
        .unwrap();
        assert_eq!(picked.gains, [1.4, 1.0, 1.6]);
        assert_eq!((picked.temperature_kelvin, picked.tint), (None, None));
        let shot = planned(&picked.layer(layer.id.clone()), AS_SHOT, json!({})).unwrap();
        assert_eq!(shot.wb_mode, WhiteBalanceMode::AsShot);
        assert_eq!(shot.as_shot_gains, original.as_shot_gains);
        let reset = planned(&shot.layer(layer.id), RESET, json!({})).unwrap();
        assert_eq!(reset, original);
    }

    #[test]
    fn direct_gain_controls_and_payload_use_the_same_finite_32_limit() {
        let matrix = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
        ];
        let original = RawPayload::for_as_shot([1.0; 3], matrix).unwrap();
        let layer = original.layer(LayerId::new());
        let maximum = planned(&layer, SET_BLUE, json!({"gain":32.0})).unwrap();
        assert_eq!(maximum.gains, [1.0, 1.0, 32.0]);
        assert!(planned(&layer, SET_BLUE, json!({"gain":32.0001})).is_err());
        assert!(planned(&layer, SET_RED, json!({"gain":f64::INFINITY})).is_err());
        let mut invalid = maximum;
        invalid.gains[2] = 32.0001;
        assert!(invalid.validate().is_err());
        for action in [SET_RED, SET_BLUE] {
            let descriptor = RawModule::new().descriptor;
            let gain = descriptor
                .actions
                .iter()
                .find(|entry| entry.id == action)
                .unwrap()
                .parameters
                .iter()
                .find(|parameter| parameter.name == "gain")
                .unwrap();
            assert!(matches!(
                gain.kind,
                ParameterKind::Number {
                    min: 0.01,
                    max: 32.0
                }
            ));
        }
    }
}
