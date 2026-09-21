//! The pixel proof module: one exact 8-bit sRGB replacement at integer content-stage coordinates,
//! the source after EXIF orientation. The host inserts the layer before the geometry tail, so the
//! quarter-turns, reflections and crop after it carry the edit instead of moving it.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    EffectDescriptor, EffectStage, ModuleDescriptor, ParameterDescriptor, ParameterKind,
    Processing, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, PIXEL_EFFECT, PixelReplace};
use serde_json::{Map, Value};

/// The decoder accepts at most 16384 pixels per side, so no stage addresses a larger coordinate.
const MAX_COORDINATE: i64 = 16383;
pub(super) const SET_PIXEL: &str = "set-pixel";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn coordinate(name: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Integer {
            min: 0,
            max: MAX_COORDINATE,
        },
        required: true,
        default: None,
        unit: Some("px".into()),
        notes: format!(
            "{name} in the content stage, the source after EXIF orientation, origin top left"
        ),
    }
}

#[derive(Debug)]
pub struct PixelModule {
    descriptor: ModuleDescriptor,
}

impl Default for PixelModule {
    fn default() -> Self {
        Self::new()
    }
}

impl PixelModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.pixel".into(),
                title: "Pixel".into(),
                hint: Some("One exact pixel".into()),
                effects: vec![EffectDescriptor {
                    id: PIXEL_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Pixel,
                }],
                actions: vec![ActionDescriptor {
                    id: SET_PIXEL.into(),
                    title: "Set pixel".into(),
                    notes: "replaces one pixel of the content stage, the source after EXIF orientation; later rotations, reflections and the crop carry the edit, and replacing a pixel with its current value is a reported no-op".into(),
                    summary: Some("Pixel {x}, {y}".into()),
                    parameters: vec![
                        coordinate("x"),
                        coordinate("y"),
                        ParameterDescriptor {
                            name: "rgb".into(),
                            kind: ParameterKind::Color,
                            required: true,
                            default: None,
                            unit: None,
                            notes: "three 8-bit sRGB channels".into(),
                        },
                    ],
                }],
                controls: vec![Control::Group {
                    label: "Pixel proof".into(),
                    reset: None,
                    controls: vec![
                        Control::Number {
                            action: SET_PIXEL.into(),
                            parameter: "x".into(),
                            label: "X".into(),
                        },
                        Control::Number {
                            action: SET_PIXEL.into(),
                            parameter: "y".into(),
                            label: "Y".into(),
                        },
                        Control::Color {
                            action: SET_PIXEL.into(),
                            parameter: "rgb".into(),
                            label: "RGB".into(),
                        },
                        Control::Action {
                            action: SET_PIXEL.into(),
                            label: "Apply pixel".into(),
                            preset: Map::new(),
                        },
                    ],
                }],
                reset: None,
                canvas: Some(CanvasInteraction::PointPick {
                    action: SET_PIXEL.into(),
                    x: "x".into(),
                    y: "y".into(),
                    title: "Pick pixel".into(),
                    shortcut: None,
                }),
                // A proof tool, not a photo-editing one.
                developer: true,
                availability: Availability::Available,
            },
        }
    }
}

fn coordinate_value(parameters: &Map<String, Value>, name: &str) -> Result<u32, Error> {
    parameters
        .get(name)
        .and_then(Value::as_i64)
        .filter(|value| (0..=MAX_COORDINATE).contains(value))
        .map(|value| value as u32)
        .ok_or_else(|| {
            validation(format!(
                "parameter {name} must be an integer within 0..={MAX_COORDINATE}"
            ))
        })
}

fn color_value(parameters: &Map<String, Value>) -> Result<[u8; 3], Error> {
    let channels = parameters
        .get("rgb")
        .and_then(Value::as_array)
        .filter(|channels| channels.len() == 3)
        .ok_or_else(|| validation("parameter rgb must be three sRGB channels 0..=255"))?;
    let mut rgb = [0u8; 3];
    for (slot, channel) in rgb.iter_mut().zip(channels) {
        *slot = u8::try_from(
            channel
                .as_u64()
                .ok_or_else(|| validation("parameter rgb must be three sRGB channels 0..=255"))?,
        )
        .map_err(|_| validation("parameter rgb must be three sRGB channels 0..=255"))?;
    }
    Ok(rgb)
}

fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<PixelReplace, Error> {
    if effect_id != PIXEL_EFFECT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("unavailable effect {effect_id}"),
        ));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("unsupported effect format {format}"),
        ));
    }
    serde_json::from_value(payload.clone())
        .map_err(|error| validation(format!("invalid pixel payload: {error}")))
}

impl ToolModule for PixelModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id != SET_PIXEL {
            return Err(validation(format!("unknown action {action_id}")));
        }
        let x = coordinate_value(parameters, "x")?;
        let y = coordinate_value(parameters, "y")?;
        let rgb = color_value(parameters)?;
        let mut stored = Map::new();
        stored.insert("x".into(), Value::from(x));
        stored.insert("y".into(), Value::from(y));
        stored.insert("rgb".into(), Value::from(rgb.to_vec()));
        Ok(ActionInput {
            action_id: SET_PIXEL.into(),
            parameters: stored,
        })
    }

    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let x = coordinate_value(&input.parameters, "x")?;
        let y = coordinate_value(&input.parameters, "y")?;
        let rgb = color_value(&input.parameters)?;
        // The coordinates address the stage this layer will be inserted at, not the output stage:
        // a pixel outside a crop is still a pixel of the photograph, and replacing one with the
        // value the content already holds is a no-op whatever a later resample shows there.
        let index = (stage.insertion_index)(EffectStage::Pixel);
        let content = (stage.stage_before)(index)?;
        let outside = || {
            validation(format!(
                "pixel ({x}, {y}) is outside the {}x{} content stage",
                content.width, content.height
            ))
        };
        if x >= content.width || y >= content.height {
            return Err(outside());
        }
        let current = (stage.sample_before)(index, x, y)?.ok_or_else(outside)?;
        if current[..3] == rgb {
            return Ok(ActionPlan::NoOp);
        }
        Ok(ActionPlan::Commit(Layer::pixel(x, y, rgb)))
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        payload(effect_id, format, value).map(|_| ())
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let pixel = payload(effect_id, format, value)?;
        Ok(format!(
            "Pixel {}, {} → {},{},{}",
            pixel.x, pixel.y, pixel.rgb[0], pixel.rgb[1], pixel.rgb[2]
        ))
    }

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        let pixel = payload(effect_id, format, value)?;
        if pixel.x >= stage.width || pixel.y >= stage.height {
            return Err(validation(format!(
                "pixel ({}, {}) is outside {}x{} input stage",
                pixel.x, pixel.y, stage.width, stage.height
            )));
        }
        Ok(Processing::PointReplace {
            x: pixel.x,
            y: pixel.y,
            rgb: pixel.rgb,
        })
    }
}
