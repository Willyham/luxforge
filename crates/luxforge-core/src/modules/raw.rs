//! The required source-stage interpretation of a RAW original: its white balance. Exposure is
//! Basic's on every kind; this module's development carries none.
//!
//! The module declares no controls, so it draws no section: Basic's White balance group carries a
//! RAW variant of each of its controls and of its reset, which reach this module's `set-raw` and
//! its sensor pick on the global target of a RAW photo ([`white_balance_variants`]).
pub(super) mod lightroom_white_balance;
pub(super) mod white_balance;
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    ControlVariant, EffectDescriptor, EffectStage, ExactGeometry, LayerReport, LayerUpdate,
    ModuleDescriptor, ParameterDescriptor, Processing, ResetAction, StageContext, ToolModule,
    decode_parameters,
};
use crate::{Error, Layer, LayerId, SourceTag};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The RAW module's one source-stage effect: the development of the RAW original, always the
/// first layer of a RAW asset's stack.
pub(super) const RAW_EFFECT: &str = "luxforge.raw";

/// The RAW development's own payload format. Every other effect stays at the shared
/// [`crate::EFFECT_FORMAT`]; a RAW layer of any other format is refused as `incompatible` and never
/// rewritten.
pub(super) const RAW_EFFECT_FORMAT: u32 = 2;

/// The RAW module's identity, which Basic's control variants name.
pub(crate) const RAW_MODULE: &str = "luxforge.raw";

/// The white-balance field patch: `temperature`, `tint` and `white-balance`.
pub(crate) const SET_RAW: &str = "set-raw";
const SET_RED: &str = "set-raw-red-gain";
const SET_BLUE: &str = "set-raw-blue-gain";
const PICK_NEUTRAL: &str = "pick-raw-neutral";
const TEMPERATURE: &str = "temperature";
const TINT: &str = "tint";
const WHITE_BALANCE: &str = "white-balance";
const AS_SHOT: &str = "as-shot";
const CUSTOM: &str = "custom";
/// The words the controls and history labels share with Basic's white balance.
const TEMPERATURE_LABEL: &str = "Temperature";
const TINT_LABEL: &str = "Tint";
const GROUP_LABEL: &str = "White balance";
const PICKER_LABEL: &str = "Neutral picker";
const AS_SHOT_LABEL: &str = "As shot";
pub(super) const MAX_RAW_GAIN: f64 = 32.0;
/// The declared defaults of the two white-balance fields: what they show, and what a custom
/// change keeps for the field it does not name, when the gains in force have no temperature and
/// tint in range.
const CUSTOM_START_KELVIN: f64 = 6504.0;
const CUSTOM_START_TINT: f64 = 0.0;

/// Whether `layer` is a RAW development: what the host's RAW source reads its one source layer by,
/// so no code outside this module names the effect.
pub(crate) fn is_raw_development(layer: &Layer) -> bool {
    layer.effect_id == RAW_EFFECT
}

/// Refuse anything but the RAW development's own effect and format: another effect is not a RAW
/// source layer, and another format is data this build does not read, refused rather than
/// rewritten.
fn raw_effect(effect_id: &str, format: u32) -> Result<(), Error> {
    if effect_id != RAW_EFFECT {
        return Err(Error::incompatible("invalid RAW source layer"));
    }
    if format != RAW_EFFECT_FORMAT {
        return Err(Error::incompatible(format!(
            "unsupported effect format {format}"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhiteBalanceMode {
    AsShot,
    Custom,
}

/// A RAW development: the white balance the mosaic is developed at, and the capture's own
/// calibration. As shot is canonical — the camera's gains and no custom temperature or tint — so
/// the Original's development is exactly [`RawPayload::for_as_shot`] and a return to As shot is
/// that payload again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPayload {
    pub wb_mode: WhiteBalanceMode,
    /// Green-normalized pre-demosaic sensor gains; the as-shot gains under As shot.
    pub gains: [f32; 3],
    /// Immutable camera as-shot gains carried by the Original layer.
    pub as_shot_gains: [f32; 3],
    /// Capture calibration, not an editable white-balance setting.
    pub cam_xyz: [[f32; 3]; 4],
    /// An explicit custom temperature and tint; `None` under As shot and for gains set another
    /// way (a neutral pick or an explicit gain).
    pub temperature_kelvin: Option<f64>,
    pub tint: Option<f64>,
}

impl Default for RawPayload {
    fn default() -> Self {
        Self {
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
    /// Monochrome sources carry no colour response, so WB cannot be an edit.
    pub fn white_balance_available(&self) -> bool {
        self.cam_xyz.iter().flatten().any(|value| *value != 0.0)
    }

    pub fn for_as_shot(gains: [f32; 3], cam_xyz: [[f32; 3]; 4]) -> Result<Self, Error> {
        let payload = Self {
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

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.gains[1] != 1.0
            || self.as_shot_gains[1] != 1.0
            || self
                .gains
                .iter()
                .chain(self.as_shot_gains.iter())
                .any(|g| !g.is_finite() || *g <= 0.0 || f64::from(*g) > MAX_RAW_GAIN)
        {
            return Err(Error::validation(
                "RAW gains must be finite, positive, <=32, and green-normalized",
            ));
        }
        if self
            .cam_xyz
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(Error::validation("RAW camera calibration must be finite"));
        }
        if !self.white_balance_available()
            && (self.wb_mode != WhiteBalanceMode::AsShot
                || self.gains != [1.0; 3]
                || self.as_shot_gains != [1.0; 3])
        {
            return Err(Error::validation(
                "white balance is unavailable for monochrome originals",
            ));
        }
        match (self.wb_mode, self.temperature_kelvin, self.tint) {
            (WhiteBalanceMode::AsShot, None, None) if self.gains == self.as_shot_gains => {}
            (WhiteBalanceMode::AsShot, ..) => {
                return Err(Error::validation(
                    "a RAW As shot development holds the camera's gains and no custom \
                     temperature or tint",
                ));
            }
            (WhiteBalanceMode::Custom, None, None) => {}
            (WhiteBalanceMode::Custom, Some(temperature), Some(tint)) => {
                let resolved =
                    white_balance::gains_from_temperature_tint(temperature, tint, self.cam_xyz)?;
                if self
                    .gains
                    .iter()
                    .zip(resolved)
                    .any(|(actual, expected)| (actual - expected).abs() > 1e-5)
                {
                    return Err(Error::validation(
                        "RAW custom temperature/tint disagree with sensor gains",
                    ));
                }
            }
            (WhiteBalanceMode::Custom, ..) => {
                return Err(Error::validation(
                    "RAW custom temperature and tint must be paired",
                ));
            }
        }
        Ok(())
    }

    /// The development a stored layer holds.
    pub(crate) fn from_layer(layer: &Layer) -> Result<Self, Error> {
        Self::from_value(&layer.effect_id, layer.effect_format, &layer.payload)
    }

    /// The development a payload stored under this effect and format holds, parsed straight from
    /// the stored value and validated.
    pub(crate) fn from_value(effect_id: &str, format: u32, payload: &Value) -> Result<Self, Error> {
        raw_effect(effect_id, format)?;
        let payload = Self::deserialize(payload)
            .map_err(|e| Error::validation(format!("invalid RAW payload: {e}")))?;
        payload.validate()?;
        Ok(payload)
    }

    /// This development as a stored payload.
    pub(crate) fn value(&self) -> Value {
        serde_json::to_value(self).expect("validated RAW payload serializes")
    }

    pub fn layer(&self, id: LayerId) -> Layer {
        Layer {
            id,
            effect_id: RAW_EFFECT.into(),
            effect_format: RAW_EFFECT_FORMAT,
            payload: self.value(),
            // Source development is the whole content stage: a mask has no stage to read here.
            mask: None,
            artifacts: Vec::new(),
        }
    }

    /// Whether this development is the Original's: As shot, which is canonical, so it equals
    /// [`RawPayload::for_as_shot`] of its own as-shot gains and calibration. Anything else is an
    /// edit.
    pub(crate) fn is_neutral(&self) -> bool {
        self.wb_mode == WhiteBalanceMode::AsShot
    }

    /// The temperature and tint whose gains these are, when a temperature in 2000..12000 K and a
    /// tint within ±100 reproduce them: a custom temperature and tint are themselves, and any other
    /// gains — the camera's under As shot, or custom gains a neutral pick or an explicit gain set —
    /// are solved from the camera matrix ([`white_balance::temperature_tint_from_gains`]).
    fn equivalent(&self) -> Option<[f64; 2]> {
        match (self.wb_mode, self.temperature_kelvin, self.tint) {
            (WhiteBalanceMode::Custom, Some(kelvin), Some(tint)) => Some([kelvin, tint]),
            _ => white_balance::temperature_tint_from_gains(self.gains, self.cam_xyz).ok(),
        }
    }

    /// The temperature and tint of the white balance in force: what the controls show, and where
    /// a custom temperature or tint change starts from for the field it does not name
    /// (`Self::equivalent`). Gains no temperature in 2000..12000 K and tint within ±100 reproduce
    /// are the declared 6504 K and 0.
    pub fn white_balance_controls(&self) -> [f64; 2] {
        self.equivalent()
            .unwrap_or([CUSTOM_START_KELVIN, CUSTOM_START_TINT])
    }
}

/// `set-raw {white-balance: as-shot}`: the camera's own white balance, which every RAW variant's
/// reset runs.
fn as_shot_reset() -> ResetAction {
    ResetAction {
        action: SET_RAW.into(),
        preset: Map::from_iter([(WHITE_BALANCE.to_owned(), Value::from(AS_SHOT))]),
    }
}

/// The RAW module's controls, as the variants Basic's White balance group declares for a RAW photo:
/// its Temperature and Tint, its picker, its As shot button and its group reset. They live here so
/// no other module spells this module's actions or parameters.
pub(crate) struct WhiteBalanceVariants {
    pub temperature: ControlVariant,
    pub tint: ControlVariant,
    pub picker: ControlVariant,
    pub as_shot: ControlVariant,
    pub reset: ControlVariant,
}

pub(crate) fn white_balance_variants() -> WhiteBalanceVariants {
    let control = |control: Control| ControlVariant::control(SourceTag::Raw, RAW_MODULE, control);
    WhiteBalanceVariants {
        // Resetting either field returns the development to the camera's own white balance, as
        // Lightroom's Temp and Tint do, rather than switching it to a custom 6504 K.
        temperature: control(
            Control::number(SET_RAW, TEMPERATURE, TEMPERATURE_LABEL)
                .rail(crate::RailDecoration::Temperature)
                .field_reset(as_shot_reset())
                .into(),
        ),
        tint: control(
            Control::number(SET_RAW, TINT, TINT_LABEL)
                .rail(crate::RailDecoration::Tint)
                .field_reset(as_shot_reset())
                .into(),
        ),
        picker: control(Control::picker(PICKER_LABEL).into()),
        as_shot: control(
            Control::action(SET_RAW, AS_SHOT_LABEL)
                .preset(as_shot_reset().preset)
                .icon("target")
                .into(),
        ),
        reset: ControlVariant::reset(SourceTag::Raw, RAW_MODULE, as_shot_reset()),
    }
}

fn action(id: &str, title: &str, parameters: Vec<ParameterDescriptor>) -> ActionDescriptor {
    ActionDescriptor {
        parameters,
        ..ActionDescriptor::new(
            id,
            title,
            "updates the required RAW source layer through ordinary history",
        )
    }
}

/// A gain request, which the generic check has already validated.
#[derive(Deserialize)]
struct Gain {
    gain: f32,
}

/// A neutral pick's upright content coordinate, which the generic check has already validated.
#[derive(Deserialize)]
struct Point {
    x: u32,
    y: u32,
}

/// A field-patch request's number, which the generic check has already validated.
fn number(parameters: &Map<String, Value>, name: &str) -> Option<f64> {
    parameters.get(name).and_then(Value::as_f64)
}

/// `Temperature 5500 K`, `Tint +12`: the words Basic's history uses for the same controls.
fn temperature_label(kelvin: f64) -> String {
    format!("{TEMPERATURE_LABEL} {kelvin:.0} K")
}

fn tint_label(tint: f64) -> String {
    format!("{TINT_LABEL} {tint:+.0}")
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
                id: RAW_MODULE.into(),
                title: "RAW".into(),
                hint: Some("Source development".into()),
                effects: vec![EffectDescriptor {
                    format: RAW_EFFECT_FORMAT,
                    sources: vec![SourceTag::Raw],
                    ..EffectDescriptor::new(RAW_EFFECT, EffectStage::Source)
                }],
                actions: vec![
                    ActionDescriptor {
                        patch: true,
                        parameters: vec![
                            ParameterDescriptor::number(
                                TEMPERATURE,
                                white_balance::MIN_TEMPERATURE_K,
                                white_balance::MAX_TEMPERATURE_K,
                            )
                            .default(CUSTOM_START_KELVIN)
                            .unit("K")
                            .step(10.0)
                            .precision(0)
                            .notes(
                                "a custom correlated colour temperature in kelvin on the camera \
                                 matrix's locus; the tint in force is kept when tint is not sent",
                            ),
                            ParameterDescriptor::number(
                                TINT,
                                white_balance::MIN_TINT,
                                white_balance::MAX_TINT,
                            )
                            .default(CUSTOM_START_TINT)
                            .step(1.0)
                            .precision(0)
                            .notes(
                                "a custom offset from the locus in Luxforge tint units of 1e-4 \
                                 CIE 1960 uv, positive magenta; the temperature in force is kept \
                                 when temperature is not sent",
                            ),
                            ParameterDescriptor::enumeration(WHITE_BALANCE, [AS_SHOT, CUSTOM])
                                .notes(
                                    "as-shot returns the development to the camera's own white \
                                     balance, the Original's; custom names a temperature or tint \
                                     sent beside it",
                                ),
                        ],
                        ..ActionDescriptor::new(
                            SET_RAW,
                            "Set RAW white balance",
                            "sets the RAW development's white balance in the required RAW \
                                source layer: temperature (K) and tint on the camera matrix's \
                                locus, or white-balance as-shot for the camera's own. A field not \
                                sent keeps the white balance in force, and custom may be sent \
                                beside temperature or tint; as-shot beside either, and custom \
                                alone, are refused",
                        )
                    },
                    action(
                        SET_RED,
                        "Red gain",
                        vec![
                            ParameterDescriptor::number("gain", 0.01, MAX_RAW_GAIN)
                                .required(true)
                                .default(1.0)
                                .unit("×")
                                .step(0.01)
                                .precision(2)
                                .notes("finite value"),
                        ],
                    ),
                    action(
                        SET_BLUE,
                        "Blue gain",
                        vec![
                            ParameterDescriptor::number("gain", 0.01, MAX_RAW_GAIN)
                                .required(true)
                                .default(1.0)
                                .unit("×")
                                .step(0.01)
                                .precision(2)
                                .notes("finite value"),
                        ],
                    ),
                    action(
                        PICK_NEUTRAL,
                        "Pick neutral patch",
                        ["x", "y"]
                            .map(|name| {
                                ParameterDescriptor::pixel_coordinate(name)
                                    .notes("upright RAW content coordinate")
                            })
                            .into(),
                    ),
                ],
                queries: Vec::new(),
                // No controls and no module reset: Basic's White balance group reaches this module's
                // actions through its RAW variants, so there is no RAW section.
                controls: Vec::new(),
                reset: None,
                canvas: Some(CanvasInteraction::PointPick {
                    action: PICK_NEUTRAL.into(),
                    x: "x".into(),
                    y: "y".into(),
                    title: PICKER_LABEL.into(),
                    // Basic's picker and its `W` reach this mode on a RAW photo's global target.
                    shortcut: None,
                    icon: None,
                    // The pick is the whole white balance: it commits at the located pixel.
                    commit: true,
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

    /// `set-raw` stores exactly the fields sent, as every patch does, after refusing the two
    /// combinations that name no one white balance: `as-shot` beside a temperature or tint, and
    /// `custom` alone.
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id == SET_RAW {
            let custom_field =
                parameters.contains_key(TEMPERATURE) || parameters.contains_key(TINT);
            match parameters.get(WHITE_BALANCE).and_then(Value::as_str) {
                Some(AS_SHOT) if custom_field => {
                    return Err(Error::validation("As shot takes no temperature or tint"));
                }
                Some(CUSTOM) if !custom_field => {
                    return Err(Error::validation(
                        "a custom white balance needs a temperature or a tint",
                    ));
                }
                _ => {}
            }
        } else if ![SET_RED, SET_BLUE, PICK_NEUTRAL].contains(&action_id) {
            return Err(Error::validation(format!("unknown RAW action {action_id}")));
        }
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }

    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        if stage.kind != SourceTag::Raw {
            return Err(Error::validation("RAW controls require a RAW original"));
        }
        let (_, layer) = stage.own_layer(RAW_EFFECT)?.ok_or_else(|| {
            Error::incompatible("RAW recipe is missing its required source layer")
        })?;
        let stored = RawPayload::from_layer(layer)?;
        if !stored.white_balance_available() {
            // Reset Basic composes the source group's As shot reset with its tone reset.
            // A monochrome original is already As shot; this component changes nothing.
            if input.action_id == SET_RAW
                && input.parameters.len() == 1
                && input
                    .parameters
                    .get("white-balance")
                    .and_then(Value::as_str)
                    == Some(AS_SHOT)
            {
                return Ok(ActionPlan::NoOp);
            }
            return Err(Error::validation(
                "white balance and neutral picker are unavailable for monochrome originals",
            ));
        }
        let mut payload = stored.clone();
        match input.action_id.as_str() {
            SET_RAW => {
                let parameters = &input.parameters;
                let temperature = number(parameters, TEMPERATURE);
                let tint = number(parameters, TINT);
                if parameters.get(WHITE_BALANCE).and_then(Value::as_str) == Some(AS_SHOT) {
                    payload = RawPayload::for_as_shot(payload.as_shot_gains, payload.cam_xyz)?;
                } else if temperature.is_some() || tint.is_some() {
                    // The field not sent keeps the white balance in force: from As shot or a
                    // neutral pick that is its equivalent, as the controls show it.
                    let [kelvin_in_force, tint_in_force] = payload.white_balance_controls();
                    let temperature = temperature.unwrap_or(kelvin_in_force);
                    let tint = tint.unwrap_or(tint_in_force);
                    payload.gains = white_balance::gains_from_temperature_tint(
                        temperature,
                        tint,
                        payload.cam_xyz,
                    )?;
                    payload.temperature_kelvin = Some(temperature);
                    payload.tint = Some(tint);
                    payload.wb_mode = WhiteBalanceMode::Custom;
                }
            }
            SET_RED | SET_BLUE => {
                let Gain { gain } = decode_parameters(&input.action_id, &input.parameters)?;
                payload.gains[if input.action_id == SET_RED { 0 } else { 2 }] = gain;
                payload.wb_mode = WhiteBalanceMode::Custom;
                payload.temperature_kelvin = None;
                payload.tint = None;
            }
            PICK_NEUTRAL => {
                let Point { x, y } = decode_parameters(&input.action_id, &input.parameters)?;
                payload.gains = stage.sensor_neutral(x, y)?;
                payload.temperature_kelvin = None;
                payload.tint = None;
                payload.wb_mode = WhiteBalanceMode::Custom;
            }
            _ => return Err(Error::validation("unknown RAW action")),
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

    /// `As shot`, or the custom white balance as its controls name it: `Temperature 5500 K · Tint
    /// +12`, the equivalent of gains a pick or an explicit gain set, and `Custom gains` for gains no
    /// temperature and tint in range reproduce. Its values are the white balance in force, named as
    /// `set-raw`'s parameters, so a client seeds Basic's RAW variants from the displayed entry
    /// without reading the payload: `white-balance` (`as-shot` or `custom`), and the `temperature`
    /// and `tint` the controls show — under As shot and after a pick, their equivalent
    /// ([`RawPayload::white_balance_controls`]); the explicit gains have no control and are not
    /// reported. It is neutral at the Original's development, As shot
    /// (`RawPayload::is_neutral`), which every RAW recipe holds from its Original on.
    fn describe(&self, effect_id: &str, format: u32, value: &Value) -> Result<LayerReport, Error> {
        let payload = RawPayload::from_value(effect_id, format, value)?;
        let neutral = payload.is_neutral();
        let [kelvin, tint] = payload.white_balance_controls();
        let summary = if neutral {
            AS_SHOT_LABEL.into()
        } else {
            match payload.equivalent() {
                Some([kelvin, tint]) => {
                    format!("{} · {}", temperature_label(kelvin), tint_label(tint))
                }
                None => "Custom gains".into(),
            }
        };
        let mode = match payload.wb_mode {
            WhiteBalanceMode::AsShot => AS_SHOT,
            WhiteBalanceMode::Custom => CUSTOM,
        };
        Ok(LayerReport {
            summary,
            values: Map::from_iter([
                (WHITE_BALANCE.to_owned(), Value::from(mode)),
                (TEMPERATURE.to_owned(), Value::from(kelvin)),
                (TINT.to_owned(), Value::from(tint)),
            ]),
            neutral,
        })
    }

    /// The labels Basic's white balance uses for the same controls: `Temperature 5500 K` and `Tint
    /// +12` for one field, `White balance` for both and for a neutral pick, and `Reset White
    /// balance` for As shot, which is the White balance group's reset.
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        match input.action_id.as_str() {
            SET_RAW => {
                let parameters = &input.parameters;
                if parameters.get(WHITE_BALANCE).and_then(Value::as_str) == Some(AS_SHOT) {
                    return format!("Reset {GROUP_LABEL}");
                }
                match (number(parameters, TEMPERATURE), number(parameters, TINT)) {
                    (Some(kelvin), None) => temperature_label(kelvin),
                    (None, Some(tint)) => tint_label(tint),
                    (Some(_), Some(_)) => GROUP_LABEL.into(),
                    (None, None) => action.title.clone(),
                }
            }
            PICK_NEUTRAL => GROUP_LABEL.into(),
            _ => action.title.clone(),
        }
    }

    /// What a preset captures: `{white-balance: as-shot}` under As shot, so the preset applies each
    /// photo's own camera white balance rather than this camera's equivalent Kelvin, and otherwise
    /// the `{temperature, tint}` in force.
    fn settings(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let payload = RawPayload::from_value(effect_id, format, value)?;
        if payload.is_neutral() {
            return Ok(as_shot_reset().preset);
        }
        let [kelvin, tint] = payload.white_balance_controls();
        Ok(Map::from_iter([
            (TEMPERATURE.to_owned(), Value::from(kelvin)),
            (TINT.to_owned(), Value::from(tint)),
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
        at: crate::CompileStage,
    ) -> Result<Processing, Error> {
        let stage = at.stage;
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
    use crate::ErrorKind;
    use crate::Stage;
    use crate::modules::ParameterKind;
    use serde_json::json;

    fn plan_of(layer: &Layer, action: &str, params: Value) -> Result<ActionPlan, Error> {
        let module = RawModule::new();
        let stage = crate::modules::FixedStage {
            neutral: Some([1.4, 1.0, 1.6]),
            ..crate::modules::FixedStage::new(Stage {
                width: 32,
                height: 32,
            })
            .of_kind(SourceTag::Raw)
        };
        let registry = crate::ModuleRegistry::builtin();
        let action_descriptor = module.descriptor().action(action).cloned();
        let checked = match action_descriptor {
            Some(declared) => crate::check_parameters(&declared, &params)?,
            None => params.as_object().cloned().unwrap_or_default(),
        };
        let input = module.parse(action, &checked)?;
        module.plan(
            &input,
            &stage.context(std::slice::from_ref(layer), &registry),
        )
    }

    fn planned(layer: &Layer, action: &str, params: Value) -> Result<RawPayload, Error> {
        match plan_of(layer, action, params)? {
            ActionPlan::Update(next) => RawPayload::from_layer(&Layer {
                payload: next.payload,
                ..layer.clone()
            }),
            _ => Err(Error::validation("expected RAW update")),
        }
    }

    fn label_of(action: &str, params: Value) -> String {
        let module = RawModule::new();
        let input = module
            .parse(action, params.as_object().expect("an object"))
            .expect("a parsed request");
        module.label(
            module.descriptor().action(action).expect("declared"),
            &input,
        )
    }

    const IDENTITY: [[f32; 3]; 4] = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
    ];

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
            .describe(&layer.effect_id, layer.effect_format, &layer.payload)
            .expect("a valid RAW payload reports its values")
            .values
    }

    fn settings_of(payload: &RawPayload) -> Map<String, Value> {
        let layer = payload.layer(LayerId::new());
        RawModule::new()
            .settings(&layer.effect_id, layer.effect_format, &layer.payload)
            .expect("a valid RAW payload reports its settings")
    }

    fn described(payload: &RawPayload) -> String {
        let layer = payload.layer(LayerId::new());
        RawModule::new()
            .describe(&layer.effect_id, layer.effect_format, &layer.payload)
            .expect("a valid RAW payload describes itself")
            .summary
    }

    /// The module declares one field patch, the two gain actions and the sensor pick, no controls,
    /// no module reset and a pick canvas without a shortcut of its own: Basic's White balance group
    /// reaches every one of them through its RAW variants.
    #[test]
    fn monochrome_is_as_shot_only_and_colour_changes_refuse() {
        let original = RawPayload::for_as_shot([1.0; 3], [[0.0; 3]; 4]).unwrap();
        let layer = original.layer(LayerId::new());
        assert!(!original.white_balance_available());
        for (action, params) in [
            (SET_RAW, json!({"temperature":5500.0})),
            (SET_RAW, json!({"tint":10.0})),
            (SET_RED, json!({"gain":2.0})),
            (SET_BLUE, json!({"gain":0.9})),
            (PICK_NEUTRAL, json!({"x":16,"y":16})),
        ] {
            let error = plan_of(&layer, action, params).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(error.detail.contains("monochrome"));
        }
        assert_eq!(
            plan_of(&layer, SET_RAW, json!({"white-balance":"as-shot"})).unwrap(),
            ActionPlan::NoOp
        );
        assert!(RawPayload::for_as_shot([2.0, 1.0, 1.0], [[0.0; 3]; 4]).is_err());
        let mut edited = original;
        edited.wb_mode = WhiteBalanceMode::Custom;
        edited.gains = [2.0, 1.0, 1.0];
        assert!(edited.validate().is_err());
    }

    #[test]
    fn the_descriptor_declares_set_raw_and_no_controls() {
        let module = RawModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert!(descriptor.controls.is_empty());
        assert_eq!(descriptor.reset, None);
        assert_eq!(descriptor.effects[0].format, RAW_EFFECT_FORMAT);
        assert_eq!(RAW_EFFECT_FORMAT, 2);
        assert!(descriptor.needs_foreign_picker());
        assert!(matches!(
            &descriptor.canvas,
            Some(CanvasInteraction::PointPick { shortcut: None, commit: true, title, .. })
                if title == PICKER_LABEL
        ));
        assert_eq!(
            descriptor
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            [SET_RAW, SET_RED, SET_BLUE, PICK_NEUTRAL]
        );
        let set_raw = descriptor.action(SET_RAW).expect("set-raw");
        assert!(set_raw.patch);
        let temperature = set_raw.parameter(TEMPERATURE).expect("temperature");
        assert!(matches!(
            temperature.kind,
            ParameterKind::Number {
                min: 2000.0,
                max: 12000.0
            }
        ));
        assert_eq!(temperature.unit.as_deref(), Some("K"));
        assert_eq!(temperature.step, Some(10.0));
        assert_eq!(temperature.precision, Some(0));
        assert_eq!(temperature.default, Some(Value::from(6504.0)));
        let tint = set_raw.parameter(TINT).expect("tint");
        assert!(matches!(
            tint.kind,
            ParameterKind::Number {
                min: -100.0,
                max: 100.0
            }
        ));
        // Tint is a unitless scale: a panel shows the bare number beside its label.
        assert_eq!(tint.unit, None);
        assert_eq!(tint.step, Some(1.0));
        assert_eq!(tint.default, Some(Value::from(0.0)));
        assert_eq!(
            set_raw
                .parameter(WHITE_BALANCE)
                .expect("white-balance")
                .kind,
            ParameterKind::Enum {
                options: vec![AS_SHOT.into(), CUSTOM.into()]
            }
        );
    }

    /// Every variant this module gives Basic is a valid control of this module, with a reset that
    /// runs As shot on each white-balance field.
    #[test]
    fn the_white_balance_variants_are_this_modules_own_controls() {
        let module = RawModule::new();
        let descriptor = module.descriptor();
        let variants = white_balance_variants();
        for variant in [
            &variants.temperature,
            &variants.tint,
            &variants.picker,
            &variants.as_shot,
        ] {
            assert_eq!(
                (variant.source, variant.module.as_str()),
                (SourceTag::Raw, RAW_MODULE)
            );
            let control = variant.control.as_deref().expect("a control variant");
            descriptor
                .check_control(control, 1)
                .expect("a control of the RAW module");
        }
        let reset = variants.reset.reset.as_ref().expect("a reset variant");
        descriptor.check_reset(Some(reset)).expect("a RAW reset");
        assert_eq!(reset, &as_shot_reset());
        assert_eq!(
            serde_json::to_value(variants.temperature.control.as_deref().unwrap()).unwrap(),
            json!({"kind": "number", "action": "set-raw", "parameter": "temperature",
                   "label": "Temperature", "rail": "temperature",
                   "reset": {"action": "set-raw", "preset": {"white-balance": "as-shot"}}})
        );
    }

    /// A RAW layer of the shared format 1 — written before exposure moved to Basic — is refused
    /// explicitly and never rewritten.
    #[test]
    fn a_format_one_raw_layer_is_refused_as_incompatible() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let module = RawModule::new();
        let mut stored = original.value();
        let error = module
            .validate_payload(RAW_EFFECT, 1, &stored)
            .expect_err("format 1");
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(error.detail, "unsupported effect format 1");
        // A payload that still holds an exposure is not this format's either.
        stored["exposure_ev"] = json!(0.5);
        assert!(
            module
                .validate_payload(RAW_EFFECT, RAW_EFFECT_FORMAT, &stored)
                .is_err()
        );
    }

    #[test]
    fn temperature_tint_and_picker_share_one_sensor_gain_payload() {
        let original = RawPayload::for_as_shot([2.0, 1.0, 1.5], IDENTITY).unwrap();
        // This synthetic camera's as-shot white has no temperature and tint in range, so a first
        // custom temperature keeps the declared 0 tint.
        assert!(
            white_balance::temperature_tint_from_gains(original.as_shot_gains, IDENTITY).is_err()
        );
        let layer = original.layer(LayerId::new());
        let temperature = planned(&layer, SET_RAW, json!({"temperature": 5500.0})).unwrap();
        assert_eq!(temperature.wb_mode, WhiteBalanceMode::Custom);
        assert_eq!(temperature.temperature_kelvin, Some(5500.0));
        assert_eq!(temperature.tint, Some(CUSTOM_START_TINT));
        let tint = planned(
            &temperature.layer(layer.id.clone()),
            SET_RAW,
            json!({"tint": 12.0}),
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
        // As shot is the Original's development exactly: nothing a custom change kept survives.
        let shot = planned(
            &picked.layer(layer.id.clone()),
            SET_RAW,
            json!({"white-balance": "as-shot"}),
        )
        .unwrap();
        assert_eq!(shot, original);
    }

    #[test]
    fn direct_gain_controls_and_payload_use_the_same_finite_32_limit() {
        let original = RawPayload::for_as_shot([1.0; 3], IDENTITY).unwrap();
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
                .action(action)
                .unwrap()
                .parameter("gain")
                .unwrap()
                .clone();
            assert!(matches!(
                gain.kind,
                ParameterKind::Number {
                    min: 0.01,
                    max: 32.0
                }
            ));
        }
    }

    /// The values a RAW layer reports name `set-raw`'s own parameters. Under As shot the
    /// temperature and tint are the camera's as-shot equivalent, whose gains are the as-shot
    /// gains; a custom temperature and tint are themselves; gains set another way report their
    /// equivalent, and gains no temperature and tint in range reproduce report the 6504 K and 0 a
    /// first custom adjustment starts from.
    #[test]
    fn values_report_the_white_balance_in_force() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let values = values_of(&original);
        assert_eq!(
            values.keys().collect::<Vec<_>>(),
            [TEMPERATURE, TINT, WHITE_BALANCE],
            "{values:?}"
        );
        assert_eq!(values[WHITE_BALANCE], json!("as-shot"));
        let kelvin = values[TEMPERATURE].as_f64().unwrap();
        let tint = values[TINT].as_f64().unwrap();
        let back = white_balance::gains_from_temperature_tint(kelvin, tint, Z6_CAM_XYZ).unwrap();
        for (back, shot) in back.iter().zip(Z6_AS_SHOT) {
            assert!((back - shot).abs() <= 1.0e-6 * shot, "{back} vs {shot}");
        }
        assert!((kelvin - 4_860.955).abs() < 0.001, "{kelvin}");
        assert!((tint + 49.974).abs() < 0.001, "{tint}");

        let layer = original.layer(LayerId::new());
        let custom = planned(&layer, SET_RAW, json!({"temperature": 3500.0})).unwrap();
        let values = values_of(&custom);
        assert_eq!(values[WHITE_BALANCE], json!("custom"));
        assert_eq!(
            (values[TEMPERATURE].as_f64(), values[TINT].as_f64()),
            (Some(3500.0), Some(tint)),
            "a temperature changed from As shot keeps the as-shot tint"
        );

        // A neutral pick stores gains alone; they report the temperature and tint whose gains
        // they are.
        let picked = planned(&layer, PICK_NEUTRAL, json!({"x": 1, "y": 2})).unwrap();
        let values = values_of(&picked);
        let (picked_kelvin, picked_tint) = (
            values[TEMPERATURE].as_f64().unwrap(),
            values[TINT].as_f64().unwrap(),
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

        // An identity camera whose as-shot white is far bluer than 12000 K has no equivalent.
        let blue = RawPayload::for_as_shot([2.0, 1.0, 0.2], IDENTITY).unwrap();
        assert!(white_balance::temperature_tint_from_gains(blue.as_shot_gains, IDENTITY).is_err());
        let values = values_of(&blue);
        assert_eq!(
            (values[TEMPERATURE].as_f64(), values[TINT].as_f64()),
            (Some(CUSTOM_START_KELVIN), Some(CUSTOM_START_TINT))
        );
        // And custom gains without an equivalent report the same declared values.
        let unreachable = RawPayload {
            wb_mode: WhiteBalanceMode::Custom,
            ..blue
        };
        let values = values_of(&unreachable);
        assert_eq!(
            (values[TEMPERATURE].as_f64(), values[TINT].as_f64()),
            (Some(CUSTOM_START_KELVIN), Some(CUSTOM_START_TINT))
        );
    }

    /// A custom temperature or tint change keeps the white balance in force for the field it does
    /// not name, as Lightroom does: from As shot the camera's own equivalent — also after a return
    /// to As shot — from a neutral pick that pick's equivalent, from a custom temperature and tint
    /// the stored one; and the declared 6504 K or 0 when the gains in force have no equivalent in
    /// range.
    #[test]
    fn a_custom_change_keeps_the_other_field_of_the_white_balance_in_force() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let [shot_kelvin, shot_tint] =
            white_balance::temperature_tint_from_gains(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let layer = original.layer(LayerId::new());
        let kelvin = |payload: &RawPayload| payload.temperature_kelvin.unwrap();
        let tint = |payload: &RawPayload| payload.tint.unwrap();

        let warmer = planned(&layer, SET_RAW, json!({"temperature": 3500.0})).unwrap();
        assert_eq!((kelvin(&warmer), tint(&warmer)), (3500.0, shot_tint));
        assert_eq!(
            warmer.gains,
            white_balance::gains_from_temperature_tint(3500.0, shot_tint, Z6_CAM_XYZ).unwrap()
        );
        let greener = planned(&layer, SET_RAW, json!({"tint": -60.0})).unwrap();
        assert_eq!((kelvin(&greener), tint(&greener)), (shot_kelvin, -60.0));
        // `custom` may be sent beside the field it names.
        let named = planned(
            &layer,
            SET_RAW,
            json!({"white-balance": "custom", "tint": -60.0}),
        )
        .unwrap();
        assert_eq!(named, greener);

        // From a custom temperature and tint, the stored one is kept.
        let custom = planned(
            &warmer.layer(layer.id.clone()),
            SET_RAW,
            json!({"tint": 20.0}),
        )
        .unwrap();
        assert_eq!((kelvin(&custom), tint(&custom)), (3500.0, 20.0));
        // Both fields at once set exactly them.
        let both = planned(
            &layer,
            SET_RAW,
            json!({"temperature": 3500.0, "tint": 20.0}),
        )
        .unwrap();
        assert_eq!(both, custom);

        // Back at As shot, the as-shot equivalent is in force again.
        let back = planned(
            &custom.layer(layer.id.clone()),
            SET_RAW,
            json!({"white-balance": "as-shot"}),
        )
        .unwrap();
        assert_eq!(back, original);
        let again = planned(&back.layer(layer.id.clone()), SET_RAW, json!({"tint": 5.0})).unwrap();
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
            SET_RAW,
            json!({"temperature": 5000.0}),
        )
        .unwrap();
        assert_eq!(
            (kelvin(&after_pick), tint(&after_pick)),
            (5000.0, picked_tint)
        );

        // Gains with no equivalent in range fall back to the declared 6504 K and 0.
        let unreachable = RawPayload::for_as_shot([2.0, 1.0, 0.2], IDENTITY).unwrap();
        let layer = unreachable.layer(LayerId::new());
        let warmer = planned(&layer, SET_RAW, json!({"temperature": 5000.0})).unwrap();
        assert_eq!(
            (kelvin(&warmer), tint(&warmer)),
            (5000.0, CUSTOM_START_TINT)
        );
        let greener = planned(&layer, SET_RAW, json!({"tint": -10.0})).unwrap();
        assert_eq!(
            (kelvin(&greener), tint(&greener)),
            (CUSTOM_START_KELVIN, -10.0)
        );
    }

    /// `as-shot` beside a temperature or tint, and `custom` alone, name no one white balance and
    /// are refused before anything is planned; an out-of-range field is the generic check's.
    #[test]
    fn set_raw_refuses_white_balance_combinations_that_name_no_one_white_balance() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let layer = original.layer(LayerId::new());
        for (request, detail) in [
            (
                json!({"white-balance": "as-shot", "temperature": 5000.0}),
                "As shot takes no temperature or tint",
            ),
            (
                json!({"white-balance": "as-shot", "tint": 5.0}),
                "As shot takes no temperature or tint",
            ),
            (
                json!({"white-balance": "custom"}),
                "a custom white balance needs a temperature or a tint",
            ),
        ] {
            let error = plan_of(&layer, SET_RAW, request.clone()).expect_err("refused");
            assert_eq!(error.kind, ErrorKind::Validation, "{request}");
            assert_eq!(error.detail, detail, "{request}");
        }
        for request in [
            json!({"temperature": 1999.0}),
            json!({"temperature": 12001.0}),
            json!({"tint": 101.0}),
            json!({"white-balance": "auto"}),
        ] {
            let error = plan_of(&layer, SET_RAW, request.clone()).expect_err("out of range");
            assert_eq!(error.kind, ErrorKind::Validation, "{request}");
        }
        // Each removed action is unknown.
        for removed in [
            "set-raw-exposure",
            "set-raw-temperature",
            "set-raw-tint",
            "use-as-shot-wb",
            "reset-raw",
        ] {
            assert!(RawModule::new().descriptor().action(removed).is_none());
        }
    }

    /// As shot on As shot, an empty patch and a value that is already in force change nothing.
    #[test]
    fn set_raw_reports_no_ops() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let layer = original.layer(LayerId::new());
        for request in [json!({"white-balance": "as-shot"}), json!({})] {
            assert_eq!(
                plan_of(&layer, SET_RAW, request.clone()).unwrap(),
                ActionPlan::NoOp,
                "{request}"
            );
        }
        let custom = planned(
            &layer,
            SET_RAW,
            json!({"temperature": 5000.0, "tint": 10.0}),
        )
        .unwrap()
        .layer(layer.id.clone());
        assert_eq!(
            plan_of(&custom, SET_RAW, json!({"tint": 10.0})).unwrap(),
            ActionPlan::NoOp
        );
    }

    /// A development is neutral exactly when it is As shot, which equals the Original's payload;
    /// a custom temperature or tint, a neutral pick and explicit gains are edits, and As shot
    /// returns each of them to the Original's payload exactly.
    #[test]
    fn a_development_is_neutral_exactly_at_the_originals_as_shot() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        assert!(original.is_neutral());
        let layer = original.layer(LayerId::new());
        let module = RawModule::new();
        for (action, params) in [
            (SET_RAW, json!({"temperature": 5000.0})),
            (SET_RAW, json!({"tint": 12.0})),
            (PICK_NEUTRAL, json!({"x": 1, "y": 2})),
            (SET_RED, json!({"gain": 2.0})),
        ] {
            let edited = planned(&layer, action, params).unwrap();
            assert!(!edited.is_neutral(), "{action}");
            let stored = edited.layer(layer.id.clone());
            assert!(
                !module
                    .describe(&stored.effect_id, stored.effect_format, &stored.payload)
                    .unwrap()
                    .neutral
            );
            let back = planned(&stored, SET_RAW, json!({"white-balance": "as-shot"})).unwrap();
            assert_eq!(back, original, "{action}");
            assert!(back.is_neutral(), "{action}");
        }
        // A non-canonical As shot payload is refused rather than read as neutral.
        let kept = RawPayload {
            temperature_kelvin: Some(5000.0),
            tint: Some(0.0),
            ..original.clone()
        };
        assert!(kept.validate().is_err());
        let moved = RawPayload {
            gains: [2.0, 1.0, 1.5],
            ..original
        };
        assert!(moved.validate().is_err());
    }

    /// History labels use the words Basic's white balance uses for the same controls.
    #[test]
    fn labels_name_the_white_balance_as_basic_does() {
        for (action, request, label) in [
            (
                SET_RAW,
                json!({"temperature": 5500.0}),
                "Temperature 5500 K",
            ),
            (SET_RAW, json!({"tint": 12.0}), "Tint +12"),
            (SET_RAW, json!({"tint": -7.0}), "Tint -7"),
            (
                SET_RAW,
                json!({"white-balance": "custom", "temperature": 3200.0}),
                "Temperature 3200 K",
            ),
            (
                SET_RAW,
                json!({"temperature": 5500.0, "tint": 12.0}),
                "White balance",
            ),
            (
                SET_RAW,
                json!({"white-balance": "as-shot"}),
                "Reset White balance",
            ),
            (PICK_NEUTRAL, json!({"x": 1, "y": 2}), "White balance"),
            (SET_RED, json!({"gain": 2.0}), "Red gain"),
        ] {
            assert_eq!(label_of(action, request.clone()), label, "{request}");
        }
    }

    /// The recipe row names As shot, or the custom white balance as its controls show it.
    #[test]
    fn a_layer_describes_as_shot_or_its_temperature_and_tint() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        assert_eq!(described(&original), "As shot");
        let layer = original.layer(LayerId::new());
        let custom = planned(
            &layer,
            SET_RAW,
            json!({"temperature": 5500.0, "tint": 12.0}),
        )
        .unwrap();
        assert_eq!(described(&custom), "Temperature 5500 K · Tint +12");
        let picked = planned(&layer, PICK_NEUTRAL, json!({"x": 1, "y": 2})).unwrap();
        let [kelvin, tint] = picked.white_balance_controls();
        assert_eq!(
            described(&picked),
            format!("Temperature {kelvin:.0} K · Tint {tint:+.0}")
        );
        let unreachable = RawPayload {
            wb_mode: WhiteBalanceMode::Custom,
            ..RawPayload::for_as_shot([2.0, 1.0, 0.2], IDENTITY).unwrap()
        };
        assert_eq!(described(&unreachable), "Custom gains");
    }

    /// A preset captures As shot as As shot, so it applies each photo's own camera white balance,
    /// and a custom or picked white balance as the temperature and tint in force.
    #[test]
    fn settings_narrow_as_shot_and_capture_a_custom_pair() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        assert_eq!(
            Value::Object(settings_of(&original)),
            json!({"white-balance": "as-shot"})
        );
        let layer = original.layer(LayerId::new());
        let custom = planned(
            &layer,
            SET_RAW,
            json!({"temperature": 5500.0, "tint": 12.0}),
        )
        .unwrap();
        assert_eq!(
            Value::Object(settings_of(&custom)),
            json!({"temperature": 5500.0, "tint": 12.0})
        );
        let picked = planned(&layer, PICK_NEUTRAL, json!({"x": 1, "y": 2})).unwrap();
        let [kelvin, tint] = picked.white_balance_controls();
        assert_eq!(
            Value::Object(settings_of(&picked)),
            json!({"temperature": kelvin, "tint": tint})
        );
        // Each is a request set-raw accepts.
        let declared = RawModule::new()
            .descriptor()
            .action(SET_RAW)
            .unwrap()
            .clone();
        for payload in [&original, &custom, &picked] {
            crate::check_parameters(&declared, &Value::Object(settings_of(payload)))
                .expect("a valid set-raw request");
        }
    }

    /// The plan reads the photo's kind from its context and refuses anything but a RAW photo.
    #[test]
    fn a_plan_on_a_jpeg_photo_is_refused() {
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let layer = original.layer(LayerId::new());
        let module = RawModule::new();
        let stage = crate::modules::FixedStage::new(Stage {
            width: 32,
            height: 32,
        });
        let registry = crate::ModuleRegistry::builtin();
        let input = module
            .parse(SET_RAW, json!({"tint": 5.0}).as_object().unwrap())
            .unwrap();
        let error = module
            .plan(
                &input,
                &stage.context(std::slice::from_ref(&layer), &registry),
            )
            .expect_err("a JPEG context");
        assert_eq!(error.detail, "RAW controls require a RAW original");
    }
}
