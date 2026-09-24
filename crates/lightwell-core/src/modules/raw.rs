//! The required source-stage interpretation of a RAW original.
pub mod neutral;
pub mod white_balance;
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    EffectDescriptor, EffectStage, ExactGeometry, LayerUpdate, ModuleDescriptor,
    ParameterDescriptor, ParameterKind, Processing, ResetAction, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The RAW module's one source-stage effect: the development of the RAW original, always the
/// first layer of a RAW asset's stack.
pub const RAW_EFFECT: &str = "lightwell.raw";

const SET_EXPOSURE: &str = "set-raw-exposure";
const SET_RED: &str = "set-raw-red-gain";
const SET_BLUE: &str = "set-raw-blue-gain";
const SET_TEMPERATURE: &str = "set-raw-temperature";
const SET_TINT: &str = "set-raw-tint";
const PICK_NEUTRAL: &str = "pick-raw-neutral";
const AS_SHOT: &str = "use-as-shot-wb";
const RESET: &str = "reset-raw";
pub const MAX_RAW_GAIN: f64 = 32.0;
const MIN_EXPOSURE_EV: f64 = -5.0;
const MAX_EXPOSURE_EV: f64 = 5.0;
/// The declared defaults of the two white-balance controls: what they show, and what a custom
/// change keeps for the field it does not name, when the gains in force have no temperature and
/// tint in range.
const CUSTOM_START_KELVIN: f64 = 6504.0;
const CUSTOM_START_TINT: f64 = 0.0;

fn validation(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, message)
}

/// Refuse anything but the RAW development's own effect and format.
fn raw_effect(effect_id: &str, format: u32) -> Result<(), Error> {
    if effect_id != RAW_EFFECT || format != EFFECT_FORMAT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            "invalid RAW source layer",
        ));
    }
    Ok(())
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
        if !self.exposure_ev.is_finite()
            || !(MIN_EXPOSURE_EV..=MAX_EXPOSURE_EV).contains(&self.exposure_ev)
        {
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

    /// The development a stored layer holds.
    pub fn from_layer(layer: &Layer) -> Result<Self, Error> {
        Self::from_value(&layer.effect_id, layer.effect_format, &layer.payload)
    }

    /// The development a payload stored under this effect and format holds, parsed straight from
    /// the stored value and validated.
    pub fn from_value(effect_id: &str, format: u32, payload: &Value) -> Result<Self, Error> {
        raw_effect(effect_id, format)?;
        let payload = Self::deserialize(payload)
            .map_err(|e| validation(format!("invalid RAW payload: {e}")))?;
        payload.validate()?;
        Ok(payload)
    }

    /// This development as a stored payload.
    pub fn value(&self) -> Value {
        serde_json::to_value(self).expect("validated RAW payload serializes")
    }

    pub fn layer(&self, id: LayerId) -> Layer {
        Layer {
            id,
            effect_id: RAW_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: self.value(),
            // Source development is the whole content stage: a mask has no stage to read here.
            mask: None,
            artifacts: Vec::new(),
        }
    }

    /// Whether this development is the Original's: As shot at 0 EV. The custom gains, temperature
    /// and tint a payload keeps after a return to As shot are unused while As shot is selected,
    /// and the as-shot gains and camera calibration are the capture's own, so such a layer
    /// develops exactly as [`RawPayload::for_as_shot`] does. Anything else is an edit.
    pub fn is_neutral(&self) -> bool {
        self.exposure_ev == 0.0 && self.wb_mode == WhiteBalanceMode::AsShot
    }

    /// The temperature and tint of the white balance in force: what the development's controls
    /// show, and where a custom temperature or tint change starts from for the field it does not
    /// name. A custom temperature and tint are themselves. Any other gains — the camera's under As
    /// shot, whatever custom values a return to As shot left in the payload, or custom gains a
    /// neutral pick or an explicit gain set — are the temperature and tint whose gains they are
    /// ([`white_balance::temperature_tint_from_gains`]), so the controls say what is in force and
    /// the next drag moves one field from there. Gains no temperature in 2000..12000 K and tint
    /// within ±100 reproduce are the declared 6504 K and 0.
    pub fn white_balance_controls(&self) -> [f64; 2] {
        let equivalent = |gains| {
            white_balance::temperature_tint_from_gains(gains, self.cam_xyz)
                .unwrap_or([CUSTOM_START_KELVIN, CUSTOM_START_TINT])
        };
        match (self.wb_mode, self.temperature_kelvin, self.tint) {
            (WhiteBalanceMode::AsShot, _, _) => equivalent(self.as_shot_gains),
            (WhiteBalanceMode::Custom, Some(kelvin), Some(tint)) => [kelvin, tint],
            (WhiteBalanceMode::Custom, _, _) => equivalent(self.gains),
        }
    }
}

/// One declared number parameter, with the increment and the display precision a client should
/// use for it. Both are hints: the host stores what it is sent and never rounds a request to them.
fn number(
    name: &str,
    min: f64,
    max: f64,
    default: f64,
    unit: Option<&str>,
    step: f64,
    precision: u8,
) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number { min, max },
        required: true,
        default: Some(Value::from(default)),
        unit: unit.map(Into::into),
        step: Some(step),
        precision: Some(precision),
        notes: "finite value".into(),
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
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
        step: None,
        precision: None,
        notes: "upright RAW content coordinate".into(),
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
    }
}

/// The field reset of the custom temperature and tint: the camera's own white balance.
fn as_shot_reset() -> ResetAction {
    ResetAction {
        action: AS_SHOT.into(),
        preset: Map::new(),
    }
}

fn action(id: &str, title: &str, parameters: Vec<ParameterDescriptor>) -> ActionDescriptor {
    ActionDescriptor {
        id: id.into(),
        title: title.into(),
        notes: "updates the required RAW source layer through ordinary history".into(),
        summary: None,
        patch: false,
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
                    order: 0,
                    maskable: false,
                    artifacts: false,
                    single: false,
                }],
                actions: vec![
                    action(
                        SET_EXPOSURE,
                        "Exposure",
                        vec![number(
                            "ev",
                            MIN_EXPOSURE_EV,
                            MAX_EXPOSURE_EV,
                            0.0,
                            Some("EV"),
                            0.01,
                            2,
                        )],
                    ),
                    action(
                        SET_TEMPERATURE,
                        "Custom temperature",
                        vec![number(
                            "kelvin",
                            white_balance::MIN_TEMPERATURE_K,
                            white_balance::MAX_TEMPERATURE_K,
                            CUSTOM_START_KELVIN,
                            Some("K"),
                            10.0,
                            0,
                        )],
                    ),
                    action(
                        SET_TINT,
                        "Custom tint",
                        vec![number(
                            "tint",
                            white_balance::MIN_TINT,
                            white_balance::MAX_TINT,
                            CUSTOM_START_TINT,
                            None,
                            1.0,
                            0,
                        )],
                    ),
                    action(
                        SET_RED,
                        "Red gain",
                        vec![number("gain", 0.01, MAX_RAW_GAIN, 1.0, Some("×"), 0.01, 2)],
                    ),
                    action(
                        SET_BLUE,
                        "Blue gain",
                        vec![number("gain", 0.01, MAX_RAW_GAIN, 1.0, Some("×"), 0.01, 2)],
                    ),
                    action(
                        PICK_NEUTRAL,
                        "Pick neutral patch",
                        vec![coordinate("x"), coordinate("y")],
                    ),
                    action(AS_SHOT, "As shot white balance", vec![]),
                    action(RESET, "Reset RAW", vec![]),
                ],
                queries: Vec::new(),
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
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        // Resetting either white-balance field returns the development to the
                        // camera's own white balance, as Lightroom's Temp and Tint do, rather than
                        // switching it to a custom 6504 K.
                        Control::Number {
                            action: SET_TEMPERATURE.into(),
                            parameter: "kelvin".into(),
                            label: "Custom temperature".into(),
                            style: crate::NumberStyle::Slider,
                            rail: Some(crate::RailDecoration::Temperature),
                            reset: Some(as_shot_reset()),
                        },
                        Control::Number {
                            action: SET_TINT.into(),
                            parameter: "tint".into(),
                            label: "Custom tint".into(),
                            style: crate::NumberStyle::Slider,
                            rail: Some(crate::RailDecoration::Tint),
                            reset: Some(as_shot_reset()),
                        },
                        // The sensor neutral pick, beside the temperature and tint it sets.
                        Control::Picker {
                            label: "Neutral WB".into(),
                        },
                        Control::Action {
                            action: AS_SHOT.into(),
                            label: "As shot".into(),
                            preset: Map::new(),
                            style: crate::ActionStyle::Default,
                            icon: Some("target".into()),
                        },
                    ],
                    collapsed: false,
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
                    shortcut: Some("N".into()),
                }),
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
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
        let stored = RawPayload::from_layer(layer)?;
        let mut payload = stored.clone();
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
                // The field this action does not name keeps the white balance in force: from As
                // shot or a neutral pick that is its equivalent, as the controls show it.
                let [kelvin_in_force, tint_in_force] = payload.white_balance_controls();
                let temperature = if input.action_id == SET_TEMPERATURE {
                    input
                        .parameters
                        .get("kelvin")
                        .and_then(Value::as_f64)
                        .ok_or_else(|| validation("missing Kelvin"))?
                } else {
                    kelvin_in_force
                };
                let tint = if input.action_id == SET_TINT {
                    input
                        .parameters
                        .get("tint")
                        .and_then(Value::as_f64)
                        .ok_or_else(|| validation("missing tint"))?
                } else {
                    tint_in_force
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
                payload.gains = stage.sensor_neutral(x, y)?;
                payload.temperature_kelvin = None;
                payload.tint = None;
                payload.wb_mode = WhiteBalanceMode::Custom;
            }
            AS_SHOT => payload.wb_mode = WhiteBalanceMode::AsShot,
            RESET => payload = RawPayload::for_as_shot(payload.as_shot_gains, payload.cam_xyz)?,
            _ => return Err(validation("unknown RAW action")),
        }
        payload.validate()?;
        if payload == stored {
            return Ok(ActionPlan::NoOp);
        }
        Ok(ActionPlan::Update(LayerUpdate::new(
            layer.id.clone(),
            payload.value(),
        )))
    }
    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        RawPayload::from_value(effect_id, format, value).map(|_| ())
    }
    /// The Original's development: As shot at 0 EV, whatever custom values the payload keeps
    /// ([`RawPayload::is_neutral`]). Every RAW recipe holds this layer from its Original on.
    fn is_neutral(&self, effect_id: &str, format: u32, value: &Value) -> Result<bool, Error> {
        Ok(RawPayload::from_value(effect_id, format, value)?.is_neutral())
    }
    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let payload = RawPayload::from_value(effect_id, format, value)?;
        Ok(format!(
            "Exposure {:+.2} EV · {} WB",
            payload.exposure_ev,
            match payload.wb_mode {
                WhiteBalanceMode::AsShot => "as-shot",
                WhiteBalanceMode::Custom => "custom",
            }
        ))
    }
    /// The development's exposure, temperature and tint, named as the single parameters of
    /// `set-raw-exposure`, `set-raw-temperature` and `set-raw-tint`, so a client seeds those
    /// controls from the displayed entry without reading the payload. Under As shot the
    /// temperature and tint are the camera's as-shot equivalent
    /// ([`RawPayload::white_balance_controls`]); the explicit gains have no control and are not
    /// reported, since both gain actions name their one parameter `gain`.
    fn values(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let payload = RawPayload::from_value(effect_id, format, value)?;
        let [kelvin, tint] = payload.white_balance_controls();
        Ok(Map::from_iter([
            ("ev".to_owned(), Value::from(payload.exposure_ev)),
            ("kelvin".to_owned(), Value::from(kelvin)),
            ("tint".to_owned(), Value::from(tint)),
        ]))
    }
    /// The development happens on the source before any layer is evaluated, so at compile its
    /// layer is the identity of its stage. Admission validated the payload, and the development
    /// reads it where it runs, so compiling checks only which effect and format this is: nothing
    /// is parsed and no white-balance locus is solved for an answer the payload cannot change.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        _: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        raw_effect(effect_id, format)?;
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
        let stage = crate::modules::FixedStage {
            neutral: Some([1.4, 1.0, 1.6]),
            ..crate::modules::FixedStage::new(Stage {
                width: 32,
                height: 32,
            })
        };
        let registry = crate::ModuleRegistry::builtin();
        let input = module.parse(action, params.as_object().unwrap())?;
        match module.plan(
            &input,
            &stage.context(std::slice::from_ref(layer), &registry),
        )? {
            ActionPlan::Update(next) => RawPayload::from_layer(&Layer {
                payload: next.payload,
                ..layer.clone()
            }),
            _ => Err(validation("expected RAW update")),
        }
    }

    /// The custom temperature and tint sliders declare the same rail hints as Basic's white
    /// balance, so a client colours both consistently.
    #[test]
    fn custom_temperature_and_tint_declare_their_rail_hints() {
        let module = RawModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        let group = descriptor
            .controls
            .first()
            .expect("the RAW development group");
        let Control::Group { controls, .. } = group else {
            panic!("expected a group");
        };
        let temperature = controls
            .iter()
            .find(
                |control| matches!(control, Control::Number { parameter, .. } if parameter == "kelvin"),
            )
            .expect("custom temperature control");
        assert_eq!(
            temperature,
            &Control::Number {
                action: SET_TEMPERATURE.into(),
                parameter: "kelvin".into(),
                label: "Custom temperature".into(),
                style: crate::NumberStyle::Slider,
                rail: Some(crate::RailDecoration::Temperature),
                reset: Some(as_shot_reset()),
            }
        );
        let tint = controls
            .iter()
            .find(
                |control| matches!(control, Control::Number { parameter, .. } if parameter == "tint"),
            )
            .expect("custom tint control");
        assert_eq!(
            tint,
            &Control::Number {
                action: SET_TINT.into(),
                parameter: "tint".into(),
                label: "Custom tint".into(),
                style: crate::NumberStyle::Slider,
                rail: Some(crate::RailDecoration::Tint),
                reset: Some(as_shot_reset()),
            }
        );
        // Tint is a unitless scale: the panel shows the bare number beside its label.
        let tint_parameter = &descriptor
            .action(SET_TINT)
            .expect("custom tint action")
            .parameters[0];
        assert_eq!(tint_parameter.unit, None);
    }

    /// As shot names the crosshair raw.png draws beside its label; the picker beside it keeps the
    /// run a row of labelled buttons, so the label stays.
    #[test]
    fn as_shot_names_its_icon_beside_the_picker() {
        let module = RawModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        let Control::Group { controls, .. } = &descriptor.controls[0] else {
            panic!("expected a group");
        };
        let as_shot = controls
            .iter()
            .position(
                |control| matches!(control, Control::Action { action, .. } if action == AS_SHOT),
            )
            .expect("the As shot control");
        assert!(matches!(
            &controls[as_shot],
            Control::Action { label, icon: Some(icon), .. } if label == "As shot" && icon == "target"
        ));
        assert!(matches!(
            &controls[as_shot - 1],
            Control::Picker { label } if label == "Neutral WB"
        ));
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
        // This synthetic camera's as-shot white has no temperature and tint in range, so a first
        // custom temperature keeps the declared 0 tint.
        assert!(
            white_balance::temperature_tint_from_gains(original.as_shot_gains, matrix).is_err()
        );
        let layer = original.layer(LayerId::new());
        let temperature = planned(&layer, SET_TEMPERATURE, json!({"kelvin":5500.0})).unwrap();
        assert_eq!(temperature.wb_mode, WhiteBalanceMode::Custom);
        assert_eq!(temperature.temperature_kelvin, Some(5500.0));
        assert_eq!(temperature.tint, Some(CUSTOM_START_TINT));
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

    /// The Nikon Z6's camera matrix and as-shot gains, from the supplied NEF's metadata.
    const Z6_CAM_XYZ: [[f32; 3]; 4] = [
        [0.9943, -0.3269, -0.0839],
        [-0.5323, 1.3269, 0.2259],
        [-0.1198, 0.2083, 0.7557],
        [0.0; 3],
    ];
    const Z6_AS_SHOT: [f32; 3] = [1.683_593_8, 1.0, 1.345_703_1];

    fn values_of(payload: &RawPayload) -> Map<String, Value> {
        let layer = payload.layer(LayerId::new());
        RawModule::new()
            .values(&layer.effect_id, layer.effect_format, &layer.payload)
            .expect("a valid RAW payload reports its values")
    }

    /// Resetting the custom temperature or tint field runs As shot, declared on each control and
    /// listed with it; Exposure declares no reset, so it keeps resetting to its 0 EV default.
    #[test]
    fn the_white_balance_fields_reset_to_as_shot_and_exposure_to_its_default() {
        let module = RawModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        let listed = serde_json::to_value(descriptor).unwrap();
        let controls = listed["controls"][0]["controls"]
            .as_array()
            .expect("the RAW group's controls");
        let reset_of = |parameter: &str| {
            controls
                .iter()
                .find(|control| control["kind"] == "number" && control["parameter"] == parameter)
                .map(|control| control["reset"].clone())
                .expect("the number control")
        };
        let as_shot = serde_json::json!({"action": AS_SHOT, "preset": {}});
        assert_eq!(reset_of("kelvin"), as_shot);
        assert_eq!(reset_of("tint"), as_shot);
        assert_eq!(reset_of("ev"), Value::Null, "exposure lists no reset");
        let exposure = &descriptor.action(SET_EXPOSURE).unwrap().parameters[0];
        assert_eq!(exposure.default, Some(Value::from(0.0)));
    }

    /// The values a RAW layer reports name the three sliders' own parameters. Under As shot the
    /// temperature and tint are the camera's as-shot equivalent, whose gains are the as-shot
    /// gains; a custom temperature and tint are themselves; gains set another way, and as-shot
    /// gains no temperature and tint in range reproduce, report the 6504 K and 0 a first custom
    /// adjustment starts from.
    #[test]
    fn values_report_the_white_balance_in_force() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let values = values_of(&original);
        assert_eq!(
            values.keys().collect::<Vec<_>>(),
            ["ev", "kelvin", "tint"],
            "{values:?}"
        );
        assert_eq!(values["ev"], Value::from(0.0));
        let kelvin = values["kelvin"].as_f64().unwrap();
        let tint = values["tint"].as_f64().unwrap();
        let back = white_balance::gains_from_temperature_tint(kelvin, tint, Z6_CAM_XYZ).unwrap();
        for (back, shot) in back.iter().zip(Z6_AS_SHOT) {
            assert!((back - shot).abs() <= 1.0e-6 * shot, "{back} vs {shot}");
        }
        assert!((kelvin - 4_860.955).abs() < 0.001, "{kelvin}");
        assert!((tint + 49.974).abs() < 0.001, "{tint}");

        let layer = original.layer(LayerId::new());
        let custom = planned(
            &layer,
            SET_TEMPERATURE,
            serde_json::json!({"kelvin": 3500.0}),
        )
        .unwrap();
        let values = values_of(&custom);
        assert_eq!(
            (values["kelvin"].as_f64(), values["tint"].as_f64()),
            (Some(3500.0), Some(tint)),
            "a temperature changed from As shot keeps the as-shot tint"
        );
        // Back to As shot, the custom values the payload keeps are not what is shown.
        let back = planned(
            &custom.layer(layer.id.clone()),
            AS_SHOT,
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            back.temperature_kelvin,
            Some(3500.0),
            "the payload keeps them"
        );
        assert_eq!(values_of(&back)["kelvin"].as_f64(), Some(kelvin));
        assert_eq!(values_of(&back)["tint"].as_f64(), Some(tint));

        // A neutral pick stores gains alone; they report the temperature and tint whose gains
        // they are.
        let picked = planned(&layer, PICK_NEUTRAL, serde_json::json!({"x": 1, "y": 2})).unwrap();
        let values = values_of(&picked);
        let (picked_kelvin, picked_tint) = (
            values["kelvin"].as_f64().unwrap(),
            values["tint"].as_f64().unwrap(),
        );
        let back =
            white_balance::gains_from_temperature_tint(picked_kelvin, picked_tint, Z6_CAM_XYZ)
                .unwrap();
        for (back, picked) in back.iter().zip(picked.gains) {
            assert!(
                (back - picked).abs() <= 1.0e-6 * picked,
                "{back} vs {picked}"
            );
        }
        let exposed = planned(&layer, SET_EXPOSURE, serde_json::json!({"ev": 1.25})).unwrap();
        assert_eq!(values_of(&exposed)["ev"], Value::from(1.25));

        // An identity camera whose as-shot white is far bluer than 12000 K has no equivalent.
        let identity = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
        ];
        let blue = RawPayload::for_as_shot([2.0, 1.0, 0.2], identity).unwrap();
        assert!(white_balance::temperature_tint_from_gains(blue.as_shot_gains, identity).is_err());
        let values = values_of(&blue);
        assert_eq!(
            (values["kelvin"].as_f64(), values["tint"].as_f64()),
            (Some(CUSTOM_START_KELVIN), Some(CUSTOM_START_TINT))
        );
        // And custom gains without an equivalent report the same declared values.
        let unreachable = RawPayload {
            wb_mode: WhiteBalanceMode::Custom,
            gains: [2.0, 1.0, 0.2],
            ..blue
        };
        let values = values_of(&unreachable);
        assert_eq!(
            (values["kelvin"].as_f64(), values["tint"].as_f64()),
            (Some(CUSTOM_START_KELVIN), Some(CUSTOM_START_TINT))
        );
    }

    /// A custom temperature or tint change keeps the white balance in force for the field it does
    /// not name, as Lightroom does: from As shot the camera's own equivalent — also after a return
    /// to As shot, whatever custom values the payload kept — from a neutral pick that pick's
    /// equivalent, from a custom temperature and tint the stored one; and the declared 6504 K or 0
    /// when the gains in force have no equivalent in range.
    #[test]
    fn a_custom_change_keeps_the_other_field_of_the_white_balance_in_force() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let [shot_kelvin, shot_tint] =
            white_balance::temperature_tint_from_gains(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let layer = original.layer(LayerId::new());
        let kelvin = |payload: &RawPayload| payload.temperature_kelvin.unwrap();
        let tint = |payload: &RawPayload| payload.tint.unwrap();

        let warmer = planned(&layer, SET_TEMPERATURE, json!({"kelvin": 3500.0})).unwrap();
        assert_eq!((kelvin(&warmer), tint(&warmer)), (3500.0, shot_tint));
        assert_eq!(
            warmer.gains,
            white_balance::gains_from_temperature_tint(3500.0, shot_tint, Z6_CAM_XYZ).unwrap()
        );
        let greener = planned(&layer, SET_TINT, json!({"tint": -60.0})).unwrap();
        assert_eq!((kelvin(&greener), tint(&greener)), (shot_kelvin, -60.0));

        // From a custom temperature and tint, the stored one is kept.
        let custom = planned(
            &warmer.layer(layer.id.clone()),
            SET_TINT,
            json!({"tint": 20.0}),
        )
        .unwrap();
        assert_eq!((kelvin(&custom), tint(&custom)), (3500.0, 20.0));

        // Back at As shot the kept 3500 K and 20 are not what is in force.
        let back = planned(&custom.layer(layer.id.clone()), AS_SHOT, json!({})).unwrap();
        assert_eq!(
            (back.temperature_kelvin, back.tint),
            (Some(3500.0), Some(20.0))
        );
        let again = planned(
            &back.layer(layer.id.clone()),
            SET_TINT,
            json!({"tint": 5.0}),
        )
        .unwrap();
        assert_eq!((kelvin(&again), tint(&again)), (shot_kelvin, 5.0));

        // From a neutral pick, its own equivalent.
        let picked = planned(&layer, PICK_NEUTRAL, json!({"x": 1, "y": 2})).unwrap();
        let [picked_kelvin, picked_tint] = picked.white_balance_controls();
        assert_eq!(
            [picked_kelvin, picked_tint],
            white_balance::temperature_tint_from_gains(picked.gains, Z6_CAM_XYZ).unwrap()
        );
        let after_pick = planned(
            &picked.layer(layer.id.clone()),
            SET_TEMPERATURE,
            json!({"kelvin": 5000.0}),
        )
        .unwrap();
        assert_eq!(
            (kelvin(&after_pick), tint(&after_pick)),
            (5000.0, picked_tint)
        );

        // Gains with no equivalent in range fall back to the declared 6504 K and 0.
        let identity = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
        ];
        let unreachable = RawPayload::for_as_shot([2.0, 1.0, 0.2], identity).unwrap();
        let layer = unreachable.layer(LayerId::new());
        let warmer = planned(&layer, SET_TEMPERATURE, json!({"kelvin": 5000.0})).unwrap();
        assert_eq!(
            (kelvin(&warmer), tint(&warmer)),
            (5000.0, CUSTOM_START_TINT)
        );
        let greener = planned(&layer, SET_TINT, json!({"tint": -10.0})).unwrap();
        assert_eq!(
            (kelvin(&greener), tint(&greener)),
            (CUSTOM_START_KELVIN, -10.0)
        );
    }

    /// A development is neutral exactly when it is As shot at 0 EV, whatever custom values a
    /// return to As shot left in the payload; exposure, a custom temperature, a neutral pick and
    /// explicit gains are edits.
    #[test]
    fn a_development_is_neutral_at_as_shot_and_zero_ev() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        assert!(original.is_neutral());
        let layer = original.layer(LayerId::new());
        for (action, params) in [
            (SET_EXPOSURE, serde_json::json!({"ev": 0.35})),
            (SET_TEMPERATURE, serde_json::json!({"kelvin": 5000.0})),
            (SET_TINT, serde_json::json!({"tint": 12.0})),
            (PICK_NEUTRAL, serde_json::json!({"x": 1, "y": 2})),
            (SET_RED, serde_json::json!({"gain": 2.0})),
        ] {
            let edited = planned(&layer, action, params).unwrap();
            assert!(!edited.is_neutral(), "{action}");
            if action != SET_EXPOSURE {
                let back = planned(
                    &edited.layer(layer.id.clone()),
                    AS_SHOT,
                    serde_json::json!({}),
                )
                .unwrap();
                assert_ne!(
                    back, original,
                    "{action}: the payload keeps its custom values"
                );
                assert!(back.is_neutral(), "{action}: back at As shot and 0 EV");
            } else {
                let back = planned(
                    &edited.layer(layer.id.clone()),
                    SET_EXPOSURE,
                    serde_json::json!({"ev": 0.0}),
                )
                .unwrap();
                assert!(back.is_neutral());
            }
        }
        let exposed_as_shot = RawPayload {
            exposure_ev: -0.5,
            ..original
        };
        assert!(!exposed_as_shot.is_neutral());
    }
}
