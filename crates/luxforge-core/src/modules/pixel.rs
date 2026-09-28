//! The pixel proof module: one exact 8-bit sRGB replacement at integer content-stage coordinates,
//! the source after EXIF orientation. The host inserts the layer before the geometry tail, so the
//! quarter-turns, reflections and crop after it carry the edit instead of moving it. It is a test
//! module, as its descriptor's `developer` says, so only a developer run registers it.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    EffectDescriptor, EffectStage, LayerReport, ModuleDescriptor, NewLayer, ParameterDescriptor,
    Processing, Stage, StageContext, ToolModule, decode_parameters, label_value,
};
use crate::{EFFECT_FORMAT, Error, Layer, PixelReplace};
use serde_json::{Map, Value, json};

/// The pixel module's one effect: one replaced 8-bit sRGB pixel of the content stage.
pub const PIXEL_EFFECT: &str = "luxforge.pixel.replace";

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
                id: "luxforge.pixel".into(),
                title: "Pixel".into(),
                hint: Some("One exact pixel".into()),
                effects: vec![EffectDescriptor::new(PIXEL_EFFECT, EffectStage::Pixel)],
                actions: vec![ActionDescriptor {
                    parameters: vec![
                        ParameterDescriptor::pixel_coordinate("x").notes(
                            "x in the content stage, the source after EXIF orientation, origin \
                             top left",
                        ),
                        ParameterDescriptor::pixel_coordinate("y").notes(
                            "y in the content stage, the source after EXIF orientation, origin \
                             top left",
                        ),
                        ParameterDescriptor::color("rgb")
                            .required(true)
                            .notes("three 8-bit sRGB channels"),
                    ],
                    ..ActionDescriptor::new(
                        SET_PIXEL,
                        "Set pixel",
                        "replaces one pixel of the content stage, the source after EXIF \
                         orientation; later rotations, reflections and the crop carry the edit, \
                         and replacing a pixel with its current value is a reported no-op",
                    )
                }],
                queries: Vec::new(),
                controls: vec![
                    Control::group(
                        "Pixel proof",
                        vec![
                            Control::number(SET_PIXEL, "x", "X")
                                .number_style(crate::NumberStyle::Field)
                                .into(),
                            Control::number(SET_PIXEL, "y", "Y")
                                .number_style(crate::NumberStyle::Field)
                                .into(),
                            Control::color_field(SET_PIXEL, "rgb", "RGB")
                                .color_style(crate::ColorStyle::Fields)
                                .into(),
                            // The proof's own pick mode, reached from its panel like every other.
                            Control::picker("Pick pixel").into(),
                            Control::action(SET_PIXEL, "Apply pixel").into(),
                        ],
                    )
                    .into(),
                ],
                reset: None,
                canvas: Some(CanvasInteraction::PointPick {
                    action: SET_PIXEL.into(),
                    x: "x".into(),
                    y: "y".into(),
                    title: "Pick pixel".into(),
                    shortcut: None,
                    icon: None,
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

fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<PixelReplace, Error> {
    if effect_id != PIXEL_EFFECT {
        return Err(Error::unavailable_effect(effect_id, &[]));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::incompatible(format!(
            "unsupported effect format {format}"
        )));
    }
    serde_json::from_value(payload.clone())
        .map_err(|error| Error::validation(format!("invalid pixel payload: {error}")))
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
            return Err(Error::validation(format!("unknown action {action_id}")));
        }
        // The generic check has admitted exactly the three required fields, in range.
        Ok(ActionInput {
            action_id: SET_PIXEL.into(),
            parameters: parameters.clone(),
        })
    }

    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let PixelReplace { x, y, rgb } = decode_parameters(SET_PIXEL, &input.parameters)?;
        // The coordinates address the stage this layer will be inserted at, not the output stage:
        // a pixel outside a crop is still a pixel of the photograph, and replacing one with the
        // value the content already holds is a no-op whatever a later resample shows there.
        let index = stage.insertion_index_for(PIXEL_EFFECT);
        let content = stage.stage_before(index)?;
        let outside = || {
            Error::validation(format!(
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

    fn describe(&self, effect_id: &str, format: u32, value: &Value) -> Result<LayerReport, Error> {
        let pixel = payload(effect_id, format, value)?;
        Ok(LayerReport::new(format!(
            "Pixel {}, {} → {},{},{}",
            pixel.x, pixel.y, pixel.rgb[0], pixel.rgb[1], pixel.rgb[2]
        )))
    }

    /// `Pixel 360, 240`: the coordinates the request replaces.
    fn label(&self, _: &ActionDescriptor, input: &ActionInput) -> String {
        let x = input
            .parameters
            .get("x")
            .map_or_else(String::new, label_value);
        let y = input
            .parameters
            .get("y")
            .map_or_else(String::new, label_value);
        format!("Pixel {x}, {y}")
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
            return Err(Error::validation(format!(
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
    use crate::{GroupControl, NumberControl, NumberStyle, modules::ParameterKind};

    /// The Module panels design draws X and Y as labelled px fields: a coordinate has no useful
    /// rail. Both are integer pixels with the `px` unit, and the descriptor still validates.
    #[test]
    fn the_coordinates_are_labelled_px_fields() {
        let module = PixelModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        let Control::Group(GroupControl { controls, .. }) = &descriptor.controls[0] else {
            panic!("the pixel proof is one group");
        };
        for (index, name, label) in [(0, "x", "X"), (1, "y", "Y")] {
            let Control::Number(NumberControl {
                parameter,
                label: shown,
                style,
                rail,
                ..
            }) = &controls[index]
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
