//! The pixel proof module: one exact 8-bit sRGB replacement at integer content-stage coordinates,
//! the source after EXIF orientation. The host inserts the layer before the geometry tail, so the
//! quarter-turns, reflections and crop after it carry the edit instead of moving it.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    EffectDescriptor, EffectStage, MAX_COORDINATE, ModuleDescriptor, NewLayer, ParameterDescriptor,
    Processing, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, PixelReplace};
use serde_json::{Map, Value, json};

/// The pixel module's one effect: one replaced 8-bit sRGB pixel of the content stage.
pub const PIXEL_EFFECT: &str = "lightwell.pixel.replace";

impl Layer {
    /// A stored pixel replacement, for a stack assembled directly.
    pub fn pixel(x: u32, y: u32, rgb: [u8; 3]) -> Self {
        Self::new(PIXEL_EFFECT, pixel_payload(x, y, rgb))
    }
}

fn pixel_payload(x: u32, y: u32, rgb: [u8; 3]) -> Value {
    json!({"x": x, "y": y, "rgb": rgb})
}

pub(super) const SET_PIXEL: &str = "set-pixel";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
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
                    order: 0,
                    maskable: false,
                    artifacts: false,
                    single: false,
                }],
                actions: vec![ActionDescriptor {
                    id: SET_PIXEL.into(),
                    title: "Set pixel".into(),
                    notes: "replaces one pixel of the content stage, the source after EXIF orientation; later rotations, reflections and the crop carry the edit, and replacing a pixel with its current value is a reported no-op".into(),
                    summary: Some("Pixel {x}, {y}".into()),
                    patch: false,
parameters: vec![
                        ParameterDescriptor::pixel_coordinate("x").notes(
                            "x in the content stage, the source after EXIF orientation, origin top left",
                        ),
                        ParameterDescriptor::pixel_coordinate("y").notes(
                            "y in the content stage, the source after EXIF orientation, origin top left",
                        ),
                        ParameterDescriptor::color("rgb")
                            .required(true)
                            .notes("three 8-bit sRGB channels"),
                    ],
                }],
                queries: Vec::new(),
                controls: vec![Control::group(
                    "Pixel proof",
                    vec![
                        Control::number(SET_PIXEL, "x", "X").number_style(crate::NumberStyle::Field),
                        Control::number(SET_PIXEL, "y", "Y").number_style(crate::NumberStyle::Field),
                        Control::color_field(SET_PIXEL, "rgb", "RGB")
                            .color_style(crate::ColorStyle::Fields),
                        // The proof's own pick mode, reached from its panel like every other.
                        Control::picker("Pick pixel"),
                        Control::action(SET_PIXEL, "Apply pixel"),
                    ],
                )],
                reset: None,
                canvas: Some(CanvasInteraction::PointPick {
                    action: SET_PIXEL.into(),
                    x: "x".into(),
                    y: "y".into(),
                    title: "Pick pixel".into(),
                    shortcut: None,
                    // The pick fills the coordinates; the person submits the colour with them.
                    commit: false,
                }),
                // A proof tool, not a photo-editing one.
                developer: true,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
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
        let index = stage.insertion_index_for(PIXEL_EFFECT);
        let content = stage.stage_before(index)?;
        let outside = || {
            validation(format!(
                "pixel ({x}, {y}) is outside the {}x{} content stage",
                content.width, content.height
            ))
        };
        if x >= content.width || y >= content.height {
            return Err(outside());
        }
        let current = stage.sample_before(index, x, y)?.ok_or_else(outside)?;
        if current[..3] == rgb {
            return Ok(ActionPlan::NoOp);
        }
        Ok(ActionPlan::Commit(NewLayer::new(
            PIXEL_EFFECT,
            pixel_payload(x, y, rgb),
        )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NumberStyle, modules::ParameterKind};

    /// The Module panels design draws X and Y as labelled px fields: a coordinate has no useful
    /// rail. Both are integer pixels with the `px` unit, and the descriptor still validates.
    #[test]
    fn the_coordinates_are_labelled_px_fields() {
        let module = PixelModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        let Control::Group { controls, .. } = &descriptor.controls[0] else {
            panic!("the pixel proof is one group");
        };
        for (index, name, label) in [(0, "x", "X"), (1, "y", "Y")] {
            let Control::Number {
                parameter,
                label: shown,
                style,
                rail,
                ..
            } = &controls[index]
            else {
                panic!("{name} is a number control");
            };
            assert_eq!((parameter.as_str(), shown.as_str()), (name, label));
            assert_eq!(
                *style,
                NumberStyle::Field,
                "{name} is a field, not a slider"
            );
            assert_eq!(*rail, None);
            let declared = descriptor
                .action(SET_PIXEL)
                .and_then(|action| action.parameter(name))
                .expect("a declared coordinate");
            assert_eq!(declared.unit.as_deref(), Some("px"));
            assert!(matches!(declared.kind, ParameterKind::Integer { .. }));
        }
    }
}
