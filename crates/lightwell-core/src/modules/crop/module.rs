//! The crop module: three actions over the single crop layer of a stack.
//!
//! Every action plans against the crop layer's own input stage, which is the stage produced by the
//! layers before it, and commits a payload that [`CropPayload::output_rect`] has accepted. Planning
//! is pure geometry over one immutable stage: it never rasterizes and never samples a pixel.
use super::geometry::{BoxRect, CropPayload, CropStage, MAX_ANGLE, MIN_ANGLE};
use crate::{
    CROP_EFFECT, EFFECT_FORMAT, Error, ErrorKind, Layer,
    modules::{
        ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction,
        EffectDescriptor, EffectStage, ExactGeometry, ModuleDescriptor, ParameterDescriptor,
        ParameterKind, Processing, Resample, ResetAction, Stage, StageContext, ToolModule,
    },
};
use serde_json::{Map, Value};

pub(super) const CROP_ACTION: &str = "crop";
pub(super) const CROP_FIT_ACTION: &str = "crop-fit";
pub(super) const CROP_RESET_ACTION: &str = "crop-reset";

/// Keep the current ratio: the existing crop's output ratio, or the input stage's without one.
const FREE: &str = "free";
/// The crop layer's input stage ratio, which already reflects preceding quarter turns.
const ORIGINAL: &str = "original";
/// `aspect-width` over `aspect-height`.
const CUSTOM: &str = "custom";
/// The named presets, as width over height.
const PRESETS: [(&str, f64); 4] = [
    ("1:1", 1.0),
    ("3:2", 1.5),
    ("4:3", 4.0 / 3.0),
    ("16:9", 16.0 / 9.0),
];

/// The smallest and largest custom ratio side. A ratio is a shape, not a size, so the bounds only
/// keep the quotient finite and usable.
const MIN_ASPECT_SIDE: f64 = 0.001;
const MAX_ASPECT_SIDE: f64 = 100_000.0;

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn aspect_options() -> Vec<String> {
    let mut options = vec![FREE.to_owned(), ORIGINAL.to_owned()];
    options.extend(PRESETS.iter().map(|(name, _)| (*name).to_owned()));
    options.push(CUSTOM.to_owned());
    options
}

fn angle_parameter() -> ParameterDescriptor {
    ParameterDescriptor {
        name: "angle".into(),
        kind: ParameterKind::Number {
            min: MIN_ANGLE,
            max: MAX_ANGLE,
        },
        required: false,
        default: Some(Value::from(0.0)),
        unit: Some("deg".into()),
        step: None,
        precision: None,
        notes: "straightening angle, positive turns the image clockwise on screen".into(),
    }
}

fn rectangle_parameter(name: &str, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number { min: 0.0, max: 1.0 },
        required: true,
        default: None,
        unit: Some("box".into()),
        step: None,
        precision: None,
        notes: notes.into(),
    }
}

fn aspect_side(name: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number {
            min: MIN_ASPECT_SIDE,
            max: MAX_ASPECT_SIDE,
        },
        required: false,
        default: None,
        unit: None,
        step: None,
        precision: None,
        notes: format!(
            "the {name} of a custom ratio; required with aspect custom and rejected with any other aspect"
        ),
    }
}

fn center_parameter(name: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number { min: 0.0, max: 1.0 },
        required: false,
        default: None,
        unit: Some("box".into()),
        step: None,
        precision: None,
        notes: format!(
            "{name} of the fitted rectangle's center, normalized to the rotated box at angle; both center-x and center-y or neither"
        ),
    }
}

#[derive(Debug)]
pub struct CropModule {
    descriptor: ModuleDescriptor,
}

impl Default for CropModule {
    fn default() -> Self {
        Self::new()
    }
}

impl CropModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.crop".into(),
                title: "Crop and straighten".into(),
                hint: Some("Frame, ratio and angle".into()),
                effects: vec![EffectDescriptor {
                    id: CROP_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Geometry,
                }],
                actions: vec![
                    ActionDescriptor {
                        id: CROP_ACTION.into(),
                        title: "Crop".into(),
                        notes: "sets the straightening angle and the crop rectangle of the stack's one crop layer, updating it in place or appending it; a rectangle that would need an empty corner is rejected".into(),
                        summary: Some("Crop {angle}°".into()),
                        patch: false,
parameters: vec![
                            angle_parameter(),
                            rectangle_parameter(
                                "x",
                                "left edge, as a fraction of the rotated box width at angle, measured from the box's left edge",
                            ),
                            rectangle_parameter(
                                "y",
                                "top edge, as a fraction of the rotated box height at angle, measured from the box's top edge",
                            ),
                            rectangle_parameter(
                                "width",
                                "width as a fraction of the rotated box width at angle; x + width may not exceed 1",
                            ),
                            rectangle_parameter(
                                "height",
                                "height as a fraction of the rotated box height at angle; y + height may not exceed 1",
                            ),
                        ],
                    },
                    ActionDescriptor {
                        id: CROP_FIT_ACTION.into(),
                        title: "Fit crop to a ratio".into(),
                        notes: "commits the largest covered rectangle with the chosen ratio about the chosen center".into(),
                        summary: Some("Crop {aspect}".into()),
                        patch: false,
parameters: vec![
                            ParameterDescriptor {
                                name: "aspect".into(),
                                kind: ParameterKind::Enum {
                                    options: aspect_options(),
                                },
                                required: false,
                                default: Some(Value::from(FREE)),
                                unit: None,
                                step: None, precision: None,
notes: "free keeps the existing crop's ratio, or the input stage's without a crop; original is the input stage ratio".into(),
                            },
                            aspect_side("aspect-width"),
                            aspect_side("aspect-height"),
                            angle_parameter(),
                            center_parameter("center-x"),
                            center_parameter("center-y"),
                        ],
                    },
                    ActionDescriptor {
                        id: CROP_RESET_ACTION.into(),
                        title: "Reset crop".into(),
                        notes: "returns an existing crop layer to the neutral payload; a no-op without one".into(),
                        summary: None,
                        patch: false,
parameters: Vec::new(),
                    },
                ],
                // The section's reset is the same API action the header button calls.
                queries: Vec::new(),
                controls: Vec::new(),
                reset: Some(ResetAction {
                    action: CROP_RESET_ACTION.into(),
                    preset: Map::new(),
                }),
                canvas: Some(CanvasInteraction::CropFrame {
                    action: CROP_ACTION.into(),
                    angle: "angle".into(),
                    x: "x".into(),
                    y: "y".into(),
                    width: "width".into(),
                    height: "height".into(),
                    fit_action: CROP_FIT_ACTION.into(),
                    aspect: "aspect".into(),
                    title: "Crop".into(),
                    shortcut: Some("R".into()),
                }),
                developer: false,
                availability: Availability::Available,
            },
        }
    }
}

fn optional_number(parameters: &Map<String, Value>, name: &str) -> Result<Option<f64>, Error> {
    match parameters.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| validation(format!("parameter {name} must be a number"))),
    }
}

fn bounded(name: &str, value: f64, min: f64, max: f64) -> Result<f64, Error> {
    if value < min || value > max {
        return Err(validation(format!(
            "parameter {name} must be a number within {min}..={max}"
        )));
    }
    Ok(value)
}

fn required_number(
    parameters: &Map<String, Value>,
    name: &str,
    action: &str,
) -> Result<f64, Error> {
    optional_number(parameters, name)?.ok_or_else(|| {
        validation(format!(
            "missing required parameter {name} for action {action}"
        ))
    })
}

fn stored(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

/// The `crop` request: exactly the persisted payload.
fn crop_payload(parameters: &Map<String, Value>) -> Result<CropPayload, Error> {
    let angle = optional_number(parameters, "angle")?.unwrap_or(0.0);
    let payload = CropPayload {
        angle: bounded("angle", angle, MIN_ANGLE, MAX_ANGLE)?,
        x: required_number(parameters, "x", CROP_ACTION)?,
        y: required_number(parameters, "y", CROP_ACTION)?,
        width: required_number(parameters, "width", CROP_ACTION)?,
        height: required_number(parameters, "height", CROP_ACTION)?,
    };
    payload.validate()?;
    Ok(payload)
}

/// The `crop-fit` request after the cross-parameter checks that the generic schema cannot express.
#[derive(Clone, Debug, PartialEq)]
struct FitRequest {
    aspect: String,
    custom: Option<(f64, f64)>,
    angle: f64,
    center: Option<(f64, f64)>,
}

impl FitRequest {
    fn parse(parameters: &Map<String, Value>) -> Result<Self, Error> {
        let aspect = match parameters.get("aspect") {
            None | Some(Value::Null) => FREE.to_owned(),
            Some(value) => value
                .as_str()
                .ok_or_else(|| validation("parameter aspect must be a string"))?
                .to_owned(),
        };
        let known = aspect == FREE
            || aspect == ORIGINAL
            || aspect == CUSTOM
            || PRESETS.iter().any(|(name, _)| *name == aspect);
        if !known {
            return Err(validation(format!(
                "parameter aspect must be one of {}",
                aspect_options().join(", ")
            )));
        }
        let width = optional_number(parameters, "aspect-width")?;
        let height = optional_number(parameters, "aspect-height")?;
        let custom = match (aspect == CUSTOM, width, height) {
            (true, Some(width), Some(height)) => Some((
                bounded("aspect-width", width, MIN_ASPECT_SIDE, MAX_ASPECT_SIDE)?,
                bounded("aspect-height", height, MIN_ASPECT_SIDE, MAX_ASPECT_SIDE)?,
            )),
            (true, _, _) => {
                return Err(validation(
                    "action crop-fit needs both aspect-width and aspect-height when aspect is custom",
                ));
            }
            (false, None, None) => None,
            (false, _, _) => {
                return Err(validation(format!(
                    "action crop-fit accepts aspect-width and aspect-height only when aspect is custom, not with aspect {aspect}"
                )));
            }
        };
        let angle = bounded(
            "angle",
            optional_number(parameters, "angle")?.unwrap_or(0.0),
            MIN_ANGLE,
            MAX_ANGLE,
        )?;
        let center = match (
            optional_number(parameters, "center-x")?,
            optional_number(parameters, "center-y")?,
        ) {
            (None, None) => None,
            (Some(x), Some(y)) => Some((
                bounded("center-x", x, 0.0, 1.0)?,
                bounded("center-y", y, 0.0, 1.0)?,
            )),
            _ => {
                return Err(validation(
                    "action crop-fit needs both center-x and center-y or neither",
                ));
            }
        };
        Ok(Self {
            aspect,
            custom,
            angle,
            center,
        })
    }

    fn parameters(&self) -> Map<String, Value> {
        let mut parameters = stored([
            ("aspect", Value::from(self.aspect.as_str())),
            ("angle", Value::from(self.angle)),
        ]);
        if let Some((width, height)) = self.custom {
            parameters.insert("aspect-width".into(), Value::from(width));
            parameters.insert("aspect-height".into(), Value::from(height));
        }
        if let Some((x, y)) = self.center {
            parameters.insert("center-x".into(), Value::from(x));
            parameters.insert("center-y".into(), Value::from(y));
        }
        parameters
    }

    /// Width over height for the committed rectangle.
    fn ratio(&self, input: Stage, existing: Option<&CropPayload>) -> Result<f64, Error> {
        let stage_ratio = f64::from(input.width) / f64::from(input.height);
        match self.aspect.as_str() {
            FREE => match existing {
                // The existing crop's own output ratio, which its own angle defines.
                Some(payload) => {
                    let rect = payload.output_rect(&input_stage(input, payload.angle))?;
                    Ok(f64::from(rect.width) / f64::from(rect.height))
                }
                None => Ok(stage_ratio),
            },
            ORIGINAL => Ok(stage_ratio),
            CUSTOM => {
                let (width, height) = self.custom.ok_or_else(|| {
                    validation("aspect custom has no aspect-width and aspect-height")
                })?;
                Ok(width / height)
            }
            name => PRESETS
                .iter()
                .find(|(preset, _)| *preset == name)
                .map(|(_, ratio)| *ratio)
                .ok_or_else(|| validation(format!("unknown aspect {name}"))),
        }
    }
}

fn input_stage(stage: Stage, angle: f64) -> CropStage {
    CropStage {
        width: stage.width,
        height: stage.height,
        angle,
    }
}

fn box_rect(payload: &CropPayload, stage: &CropStage) -> BoxRect {
    let (box_width, box_height) = stage.bounding_box();
    BoxRect {
        x: payload.x * box_width,
        y: payload.y * box_height,
        width: payload.width * box_width,
        height: payload.height * box_height,
    }
}

fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<CropPayload, Error> {
    if effect_id != CROP_EFFECT {
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
        .map_err(|error| validation(format!("invalid crop payload: {error}")))
}

/// The stack's one crop layer. Two of them would each claim their own input stage, so the module
/// refuses to guess which one an action addresses.
fn locate(layers: &[Layer]) -> Result<Option<(usize, &Layer)>, Error> {
    let mut found = None;
    for (index, layer) in layers.iter().enumerate() {
        if layer.effect_id == CROP_EFFECT {
            if found.is_some() {
                return Err(validation("stack has more than one crop layer"));
            }
            found = Some((index, layer));
        }
    }
    Ok(found)
}

/// Validate coverage on the input stage and choose between updating the existing crop layer,
/// appending one and reporting a no-op. The coverage error is the one the caller sees.
fn commit(
    new: CropPayload,
    stage: &CropStage,
    existing: Option<(&Layer, CropPayload)>,
) -> Result<ActionPlan, Error> {
    new.output_rect(stage)?;
    match existing {
        Some((_, current)) if current == new => Ok(ActionPlan::NoOp),
        Some((layer, _)) => Ok(ActionPlan::Update(Layer {
            id: layer.id.clone(),
            ..Layer::crop(new)
        })),
        None if new.is_neutral() => Ok(ActionPlan::NoOp),
        None => Ok(ActionPlan::Commit(Layer::crop(new))),
    }
}

impl ToolModule for CropModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = match action_id {
            CROP_ACTION => {
                let payload = crop_payload(parameters)?;
                stored([
                    ("angle", Value::from(payload.angle)),
                    ("x", Value::from(payload.x)),
                    ("y", Value::from(payload.y)),
                    ("width", Value::from(payload.width)),
                    ("height", Value::from(payload.height)),
                ])
            }
            CROP_FIT_ACTION => FitRequest::parse(parameters)?.parameters(),
            CROP_RESET_ACTION => Map::new(),
            _ => return Err(validation(format!("unknown action {action_id}"))),
        };
        Ok(ActionInput {
            action_id: action_id.to_owned(),
            parameters,
        })
    }

    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let located = locate(context.layers)?;
        // A crop layer acts on the stage the layers before it produce, not on the final stage.
        let (stage, existing) = match located {
            Some((index, layer)) => (
                (context.stage_before)(index)?,
                Some((
                    layer,
                    payload(&layer.effect_id, layer.effect_format, &layer.payload)?,
                )),
            ),
            None => (context.stage, None),
        };
        let current = existing.as_ref().map(|(_, payload)| payload);
        match input.action_id.as_str() {
            CROP_ACTION => {
                let new = crop_payload(&input.parameters)?;
                commit(new, &input_stage(stage, new.angle), existing)
            }
            CROP_FIT_ACTION => {
                let request = FitRequest::parse(&input.parameters)?;
                let ratio = request.ratio(stage, current)?;
                let target = input_stage(stage, request.angle);
                let (box_width, box_height) = target.bounding_box();
                let center = match (request.center, current) {
                    (Some((x, y)), _) => (x * box_width, y * box_height),
                    // The existing crop's center addresses one input point; keep that point.
                    (None, Some(payload)) => {
                        let reference = input_stage(stage, payload.angle);
                        let (x, y) = box_rect(payload, &reference).center();
                        let (u, v) = reference.to_input(x, y);
                        target.to_box(u, v)
                    }
                    (None, None) => (box_width / 2.0, box_height / 2.0),
                };
                // Larger than any rectangle that fits, so fitting clamps it down to the largest
                // covered rectangle of this ratio about this center.
                let extent = 2.0 * (box_width + box_height);
                let fitted =
                    target.fit_about_center(BoxRect::from_center(center, ratio * extent, extent));
                commit(fitted.normalized(&target), &target, existing)
            }
            CROP_RESET_ACTION => match existing {
                Some((_, current)) if current.is_neutral() => Ok(ActionPlan::NoOp),
                Some((layer, _)) => Ok(ActionPlan::Update(Layer {
                    id: layer.id.clone(),
                    ..Layer::crop(CropPayload::NEUTRAL)
                })),
                None => Ok(ActionPlan::NoOp),
            },
            action_id => Err(validation(format!("unknown action {action_id}"))),
        }
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        payload(effect_id, format, value)?.validate()
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let crop = payload(effect_id, format, value)?;
        crop.validate()?;
        if crop.is_neutral() {
            return Ok("Whole image".into());
        }
        let percent = |fraction: f64| (fraction * 100.0).round();
        let angle = crop.angle;
        let straightened = if angle == 0.0 {
            String::new()
        } else {
            format!(" at {angle}°")
        };
        Ok(format!(
            "{}% × {}%{straightened}",
            percent(crop.width),
            percent(crop.height)
        ))
    }

    /// The frame this layer holds, named exactly as the `crop` action's parameters are, so a client
    /// can seed its controls from the displayed entry without parsing the payload itself.
    fn values(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let crop = payload(effect_id, format, value)?;
        crop.validate()?;
        Ok(stored([
            ("angle", Value::from(crop.angle)),
            ("x", Value::from(crop.x)),
            ("y", Value::from(crop.y)),
            ("width", Value::from(crop.width)),
            ("height", Value::from(crop.height)),
        ]))
    }

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        let payload = payload(effect_id, format, value)?;
        let crop_stage = input_stage(stage, payload.angle);
        // Coverage is validated again here, so a payload saved against a different stage fails
        // explicitly instead of rendering empty corners. A sub-pixel miss is the re-rounding of a
        // fitted rectangle at another scale, which is what a display proxy asks for, and is
        // shrunk back in rather than refused.
        let rect = payload.output_rect_covered(&crop_stage)?;
        if payload.angle == 0.0 {
            // The box is the input stage and the mapping is an integer translation.
            return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
            )));
        }
        Ok(Processing::Resample(Resample {
            inverse: crop_stage.inverse_map((rect.x as f64, rect.y as f64)),
            output_width: rect.width,
            output_height: rect.height,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT, modules::check_parameters};
    use serde_json::json;

    const INPUT: Stage = Stage {
        width: 480,
        height: 320,
    };
    /// A final stage that is not the crop layer's input stage, so a plan that read the final stage
    /// instead of `stage_before` would produce different numbers.
    const FINAL: Stage = Stage {
        width: 200,
        height: 100,
    };

    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = CropModule::new();
        let declared = module
            .descriptor()
            .action(action)
            .expect("a declared action");
        let checked = check_parameters(declared, &parameters)?;
        let input = module.parse(action, &checked)?;
        let sampler = |_: u32, _: u32| -> Result<Option<[u8; 4]>, Error> {
            panic!("planning a crop never samples a pixel")
        };
        let stage_before = |index: usize| -> Result<Stage, Error> {
            assert!(index < layers.len(), "the crop layer is in the stack");
            Ok(INPUT)
        };
        let insertion_index = |_: EffectStage| layers.len();
        let sample_before = |_: usize, _: u32, _: u32| -> Result<Option<[u8; 4]>, Error> {
            panic!("planning a crop never samples a pixel")
        };
        module.plan(
            &input,
            &StageContext {
                stage: if layers.is_empty() { INPUT } else { FINAL },
                layers,
                sampler: &sampler,
                stage_before: &stage_before,
                insertion_index: &insertion_index,
                sample_before: &sample_before,
                sensor_neutral: None,
            },
        )
    }

    fn crop_layer(payload: CropPayload) -> Layer {
        Layer::crop(payload)
    }

    fn committed(plan: ActionPlan) -> CropPayload {
        let layer = match plan {
            ActionPlan::Commit(layer) | ActionPlan::Update(layer) => layer,
            ActionPlan::NoOp => panic!("expected a committed layer, not a no-op"),
        };
        assert_eq!(layer.effect_id, CROP_EFFECT);
        assert_eq!(layer.effect_format, EFFECT_FORMAT);
        serde_json::from_value(layer.payload).expect("a crop payload")
    }

    fn close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= 1e-6,
            "{what}: {actual} is not {expected}"
        );
    }

    #[test]
    fn the_descriptor_validates_and_declares_the_shape_the_desktop_codes_against() {
        let module = CropModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid crop descriptor");
        assert_eq!(descriptor.id, "lightwell.crop");
        assert_eq!(descriptor.title, "Crop and straighten");
        assert_eq!(
            descriptor
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            [CROP_ACTION, CROP_FIT_ACTION, CROP_RESET_ACTION]
        );
        let crop = descriptor.action(CROP_ACTION).expect("the crop action");
        for name in ["x", "y", "width", "height"] {
            let parameter = crop.parameter(name).expect(name);
            assert!(parameter.required, "{name} is required");
            assert_eq!(parameter.default, None, "{name} has no default");
            assert_eq!(parameter.unit.as_deref(), Some("box"));
            assert_eq!(parameter.kind, ParameterKind::Number { min: 0.0, max: 1.0 });
        }
        let angle = crop.parameter("angle").expect("angle");
        assert!(!angle.required);
        assert_eq!(angle.default, Some(Value::from(0.0)));
        assert_eq!(angle.unit.as_deref(), Some("deg"));
        assert_eq!(
            angle.kind,
            ParameterKind::Number {
                min: -45.0,
                max: 45.0
            }
        );
        let fit = descriptor
            .action(CROP_FIT_ACTION)
            .expect("the crop-fit action");
        assert_eq!(
            fit.parameter("aspect").expect("aspect").kind,
            ParameterKind::Enum {
                options: vec![
                    "free".into(),
                    "original".into(),
                    "1:1".into(),
                    "3:2".into(),
                    "4:3".into(),
                    "16:9".into(),
                    "custom".into(),
                ]
            }
        );
        assert!(
            descriptor
                .action(CROP_RESET_ACTION)
                .expect("the reset action")
                .parameters
                .is_empty()
        );
        assert_eq!(
            descriptor.canvas,
            Some(CanvasInteraction::CropFrame {
                action: "crop".into(),
                angle: "angle".into(),
                x: "x".into(),
                y: "y".into(),
                width: "width".into(),
                height: "height".into(),
                fit_action: "crop-fit".into(),
                aspect: "aspect".into(),
                title: "Crop".into(),
                shortcut: Some("R".into()),
            })
        );
        // The former Reset crop button is the module's header reset: the same API action.
        assert!(descriptor.controls.is_empty());
        assert_eq!(
            descriptor.reset,
            Some(ResetAction {
                action: "crop-reset".into(),
                preset: Map::new(),
            })
        );
        assert_eq!(descriptor.hint.as_deref(), Some("Frame, ratio and angle"));
        assert!(!descriptor.developer);
        assert_eq!(crop.summary.as_deref(), Some("Crop {angle}°"));
        assert_eq!(fit.summary.as_deref(), Some("Crop {aspect}"));
        assert_eq!(
            descriptor
                .action(CROP_RESET_ACTION)
                .expect("the reset action")
                .summary,
            None,
            "the reset label is its title"
        );
    }

    #[test]
    fn a_crop_layer_describes_its_frame_angle_and_neutral_state() {
        let module = CropModule::new();
        let described = |payload: CropPayload| {
            module
                .describe_layer(
                    CROP_EFFECT,
                    EFFECT_FORMAT,
                    &serde_json::to_value(payload).unwrap(),
                )
                .expect("a stored crop payload")
        };
        assert_eq!(described(CropPayload::NEUTRAL), "Whole image");
        assert_eq!(
            described(CropPayload {
                angle: 0.0,
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            }),
            "50% × 50%"
        );
        assert_eq!(
            described(CropPayload {
                angle: 3.5,
                x: 0.1,
                y: 0.1,
                width: 0.605,
                height: 0.8,
            }),
            "61% × 80% at 3.5°"
        );
        assert_eq!(
            module
                .describe_layer(ORIENTATION_EFFECT, EFFECT_FORMAT, &json!({}))
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
    }

    #[test]
    fn parsing_normalizes_every_action_into_its_durable_identity_and_parameters() {
        let module = CropModule::new();
        let parsed = |action: &str, parameters: Value| -> Result<ActionInput, Error> {
            let declared = module.descriptor().action(action).expect("declared");
            module.parse(action, &check_parameters(declared, &parameters)?)
        };
        let crop = parsed(
            CROP_ACTION,
            json!({"x":0.0,"y":0.25,"width":0.5,"height":0.5}),
        )
        .unwrap();
        assert_eq!(crop.action_id, "crop");
        assert_eq!(
            Value::Object(crop.parameters),
            json!({"angle":0.0,"x":0.0,"y":0.25,"width":0.5,"height":0.5}),
            "the declared angle default is stored with the rectangle"
        );
        let fit = parsed(CROP_FIT_ACTION, json!({})).unwrap();
        assert_eq!(fit.action_id, "crop-fit");
        assert_eq!(
            Value::Object(fit.parameters),
            json!({"aspect":"free","angle":0.0})
        );
        let custom = parsed(
            CROP_FIT_ACTION,
            json!({"aspect":"custom","aspect-width":5,"aspect-height":4,"angle":-7.5,"center-x":0.4,"center-y":0.6}),
        )
        .unwrap();
        assert_eq!(
            Value::Object(custom.parameters),
            json!({"aspect":"custom","angle":-7.5,"aspect-width":5.0,"aspect-height":4.0,"center-x":0.4,"center-y":0.6})
        );
        let reset = parsed(CROP_RESET_ACTION, json!({})).unwrap();
        assert_eq!(reset.action_id, "crop-reset");
        assert_eq!(Value::Object(reset.parameters), json!({}));
        assert_eq!(
            module
                .parse("crop-something", &Map::new())
                .unwrap_err()
                .detail,
            "unknown action crop-something"
        );
    }

    #[test]
    fn cross_parameter_rules_the_schema_cannot_express_are_rejected() {
        for (case, parameters, fragment) in [
            (
                "custom without dimensions",
                json!({"aspect":"custom"}),
                "needs both aspect-width and aspect-height when aspect is custom",
            ),
            (
                "custom with one dimension",
                json!({"aspect":"custom","aspect-width":3}),
                "needs both aspect-width and aspect-height when aspect is custom",
            ),
            (
                "dimensions without custom",
                json!({"aspect":"3:2","aspect-width":3,"aspect-height":2}),
                "only when aspect is custom, not with aspect 3:2",
            ),
            (
                "dimensions with the default aspect",
                json!({"aspect-width":3,"aspect-height":2}),
                "only when aspect is custom, not with aspect free",
            ),
            (
                "one center coordinate",
                json!({"center-x":0.5}),
                "needs both center-x and center-y or neither",
            ),
            (
                "the other center coordinate",
                json!({"center-y":0.5}),
                "needs both center-x and center-y or neither",
            ),
        ] {
            let error = planned(CROP_FIT_ACTION, parameters, &[]).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    #[test]
    fn a_crop_appends_updates_in_place_and_reports_both_no_ops() {
        let neutral = planned(CROP_ACTION, json!({"x":0,"y":0,"width":1,"height":1}), &[]).unwrap();
        assert_eq!(neutral, ActionPlan::NoOp, "a neutral crop adds no layer");

        let half = json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5});
        let payload = committed(planned(CROP_ACTION, half.clone(), &[]).unwrap());
        assert_eq!(
            payload,
            CropPayload {
                angle: 0.0,
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5
            }
        );

        let existing = crop_layer(payload);
        let layers = [existing.clone(), Layer::pixel(0, 0, [1, 2, 3])];
        assert_eq!(
            planned(CROP_ACTION, half, &layers).unwrap(),
            ActionPlan::NoOp,
            "the saved payload again is a no-op"
        );
        let plan = planned(
            CROP_ACTION,
            json!({"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
            &layers,
        )
        .unwrap();
        match plan {
            ActionPlan::Update(layer) => assert_eq!(
                layer.id, existing.id,
                "an update keeps the crop layer's identity"
            ),
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn a_rectangle_that_would_need_an_empty_corner_is_rejected_by_name() {
        // At 45 degrees on a 480x320 stage the whole box is far larger than the rotated source.
        let error = planned(
            CROP_ACTION,
            json!({"angle":45,"x":0,"y":0,"width":1,"height":1}),
            &[],
        )
        .expect_err("the box corners are empty at 45 degrees");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.starts_with("crop top-left corner maps to ("),
            "{error}"
        );
        assert!(
            error.detail.contains("outside the 480x320 input stage"),
            "{error}"
        );
    }

    #[test]
    fn more_than_one_crop_layer_is_refused_rather_than_guessed() {
        let layers = [
            crop_layer(CropPayload::NEUTRAL),
            crop_layer(CropPayload::NEUTRAL),
        ];
        for action in [CROP_ACTION, CROP_FIT_ACTION, CROP_RESET_ACTION] {
            let parameters = if action == CROP_ACTION {
                json!({"x":0,"y":0,"width":0.5,"height":0.5})
            } else {
                json!({})
            };
            let error = planned(action, parameters, &layers).expect_err(action);
            assert_eq!(error.kind, ErrorKind::Validation, "{action}");
            assert_eq!(
                error.detail, "stack has more than one crop layer",
                "{action}"
            );
        }
    }

    #[test]
    fn crop_fit_commits_the_largest_covered_rectangle_of_the_requested_ratio() {
        let square = committed(planned(CROP_FIT_ACTION, json!({"aspect":"1:1"}), &[]).unwrap());
        let rect = square
            .output_rect(&input_stage(INPUT, 0.0))
            .expect("a covered square");
        assert_eq!((rect.width, rect.height), (320, 320));
        assert_eq!((rect.x, rect.y), (80, 0), "centred on the input stage");

        let wide = committed(planned(CROP_FIT_ACTION, json!({"aspect":"16:9"}), &[]).unwrap());
        let rect = wide.output_rect(&input_stage(INPUT, 0.0)).unwrap();
        assert_eq!((rect.width, rect.height), (480, 270));

        let custom = committed(
            planned(
                CROP_FIT_ACTION,
                json!({"aspect":"custom","aspect-width":2,"aspect-height":1}),
                &[],
            )
            .unwrap(),
        );
        let rect = custom.output_rect(&input_stage(INPUT, 0.0)).unwrap();
        assert_eq!((rect.width, rect.height), (480, 240));

        // Free and original both mean the whole 3:2 stage without a crop, which is neutral, so
        // there is nothing to append.
        for aspect in [FREE, ORIGINAL] {
            assert_eq!(
                planned(CROP_FIT_ACTION, json!({ "aspect": aspect }), &[]).unwrap(),
                ActionPlan::NoOp,
                "{aspect}"
            );
        }
    }

    #[test]
    fn crop_fit_keeps_the_existing_center_ratio_and_honours_an_explicit_center() {
        // A 60x80 rectangle at (240, 80): 3:4, centred at (270, 120) and far from maximal.
        let existing = CropPayload {
            angle: 0.0,
            x: 0.5,
            y: 0.25,
            width: 0.125,
            height: 0.25,
        };
        let layer = crop_layer(existing);
        let layers = [layer.clone()];
        // free grows it about its own center, keeping its 3:4 ratio.
        let kept = committed(planned(CROP_FIT_ACTION, json!({"aspect":"free"}), &layers).unwrap());
        let rect = kept.output_rect(&input_stage(INPUT, 0.0)).unwrap();
        assert_eq!((rect.width, rect.height), (180, 240), "the largest 3:4");
        assert_eq!(
            (
                rect.x + i64::from(rect.width) / 2,
                rect.y + i64::from(rect.height) / 2
            ),
            (270, 120),
            "the existing center is kept"
        );
        // Fitting again is idempotent: the rectangle is already the largest of its ratio here.
        assert_eq!(
            planned(
                CROP_FIT_ACTION,
                json!({"aspect":"free"}),
                &[crop_layer(kept)]
            )
            .unwrap(),
            ActionPlan::NoOp
        );

        // An explicit center is normalized to the rotated box, which at angle 0 is the stage.
        let placed = committed(
            planned(
                CROP_FIT_ACTION,
                json!({"aspect":"1:1","center-x":0.25,"center-y":0.5}),
                &layers,
            )
            .unwrap(),
        );
        let rect = placed.output_rect(&input_stage(INPUT, 0.0)).unwrap();
        assert_eq!(rect.width, rect.height);
        assert_eq!(
            (
                rect.x + i64::from(rect.width) / 2,
                rect.y + i64::from(rect.height) / 2
            ),
            (120, 160)
        );
    }

    #[test]
    fn crop_fit_at_an_angle_stays_covered_and_updates_the_existing_layer() {
        let layers = [crop_layer(CropPayload::NEUTRAL)];
        let plan = planned(
            CROP_FIT_ACTION,
            json!({"aspect":"16:9","angle":10}),
            &layers,
        )
        .unwrap();
        let payload = match &plan {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, layers[0].id, "the crop layer keeps its identity");
                serde_json::from_value::<CropPayload>(layer.payload.clone()).unwrap()
            }
            other => panic!("expected an update, got {other:?}"),
        };
        close(payload.angle, 10.0, "the requested angle");
        let stage = input_stage(INPUT, 10.0);
        let rect = payload
            .output_rect(&stage)
            .expect("a fitted rectangle is always covered");
        // Whole-pixel extents cannot hold an irrational ratio exactly; one pixel is the bound.
        let ratio = f64::from(rect.width) / f64::from(rect.height);
        assert!(
            (ratio - 16.0 / 9.0).abs() <= 2.0 / f64::from(rect.height),
            "the fitted ratio: {ratio} is not 16:9 within a pixel of {rect:?}"
        );
        assert!(
            rect.width < 480 && rect.height < 320,
            "straightening trims the frame: {rect:?}"
        );
        // Fitting the same request again keeps the frame. Snapping the first rectangle to whole box
        // pixels moves its center by up to half a pixel on each axis, and the second fit is the
        // largest rectangle about that moved center, so the extents change by a pixel per side at
        // most. It is stable, not cumulative: refitting does not walk the frame away.
        let again = committed(
            planned(
                CROP_FIT_ACTION,
                json!({"aspect":"16:9","angle":10}),
                &[crop_layer(payload)],
            )
            .unwrap(),
        );
        let refitted = again
            .output_rect(&stage)
            .expect("a refitted rectangle is always covered");
        assert!(
            refitted.width.abs_diff(rect.width) <= 3 && refitted.height.abs_diff(rect.height) <= 3,
            "refitting did not keep the frame: {refitted:?} against {rect:?}"
        );
    }

    #[test]
    fn crop_reset_returns_an_existing_layer_to_neutral_and_is_a_no_op_otherwise() {
        assert_eq!(
            planned(CROP_RESET_ACTION, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "no crop layer, nothing to reset"
        );
        assert_eq!(
            planned(
                CROP_RESET_ACTION,
                json!({}),
                &[crop_layer(CropPayload::NEUTRAL)]
            )
            .unwrap(),
            ActionPlan::NoOp,
            "an already neutral crop layer"
        );
        let layer = crop_layer(CropPayload {
            angle: 3.0,
            x: 0.1,
            y: 0.1,
            width: 0.5,
            height: 0.5,
        });
        match planned(CROP_RESET_ACTION, json!({}), std::slice::from_ref(&layer)).unwrap() {
            ActionPlan::Update(updated) => {
                assert_eq!(updated.id, layer.id);
                assert_eq!(
                    serde_json::from_value::<CropPayload>(updated.payload).unwrap(),
                    CropPayload::NEUTRAL
                );
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn a_payload_is_validated_and_compiled_against_its_own_stage() {
        let module = CropModule::new();
        let payload = json!({"angle":0.0,"x":0.25,"y":0.0,"width":0.5,"height":1.0});
        assert!(
            module
                .validate_payload(CROP_EFFECT, EFFECT_FORMAT, &payload)
                .is_ok()
        );
        for (case, effect, format, kind) in [
            (
                "wrong effect",
                ORIENTATION_EFFECT,
                EFFECT_FORMAT,
                ErrorKind::Incompatible,
            ),
            ("wrong format", CROP_EFFECT, 99, ErrorKind::Incompatible),
        ] {
            assert_eq!(
                module
                    .validate_payload(effect, format, &payload)
                    .unwrap_err()
                    .kind,
                kind,
                "{case}"
            );
        }
        assert_eq!(
            module
                .validate_payload(
                    CROP_EFFECT,
                    EFFECT_FORMAT,
                    &json!({"angle":0.0,"x":0.0,"y":0.0,"width":1.0,"height":1.0,"extra":1})
                )
                .unwrap_err()
                .kind,
            ErrorKind::Validation,
            "unknown fields are denied"
        );

        // Angle zero is an exact integer copy of the source rectangle.
        assert_eq!(
            module
                .compile(CROP_EFFECT, EFFECT_FORMAT, &payload, INPUT)
                .unwrap(),
            Processing::ExactGeometry(ExactGeometry::crop(120, 0, 240, 320))
        );
        // Any other angle is a resample whose inverse maps output centers back into the input.
        let angled = json!({"angle":10.0,"x":0.2,"y":0.2,"width":0.4,"height":0.4});
        let stage = input_stage(INPUT, 10.0);
        let rect = serde_json::from_value::<CropPayload>(angled.clone())
            .unwrap()
            .output_rect(&stage)
            .unwrap();
        match module
            .compile(CROP_EFFECT, EFFECT_FORMAT, &angled, INPUT)
            .unwrap()
        {
            Processing::Resample(resample) => {
                assert_eq!(
                    (resample.output_width, resample.output_height),
                    (rect.width, rect.height)
                );
                assert_eq!(
                    resample.inverse,
                    stage.inverse_map((rect.x as f64, rect.y as f64))
                );
            }
            other => panic!("expected a resample, got {other:?}"),
        }
        // A payload saved for a different stage fails explicitly rather than reading empty corners.
        let error = module
            .compile(
                CROP_EFFECT,
                EFFECT_FORMAT,
                &json!({"angle":45.0,"x":0.0,"y":0.0,"width":1.0,"height":1.0}),
                INPUT,
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("corner maps to"), "{error}");
    }

    #[test]
    fn a_crop_layer_plans_against_its_own_input_stage_not_the_final_one() {
        // The stack is a small crop followed by a transform, so the final stage (`FINAL`, 200x100,
        // ratio 2) is not the crop layer's own input stage (480x320, ratio 1.5). Fitting to
        // `original` must use the crop layer's own input stage.
        assert_ne!(
            f64::from(FINAL.width) / f64::from(FINAL.height),
            f64::from(INPUT.width) / f64::from(INPUT.height),
            "the final stage would have produced a different ratio"
        );
        let layers = [
            crop_layer(CropPayload {
                angle: 0.0,
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            }),
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            }),
        ];
        let payload =
            committed(planned(CROP_FIT_ACTION, json!({"aspect":"original"}), &layers).unwrap());
        let rect = payload
            .output_rect(&input_stage(INPUT, 0.0))
            .expect("covered on the crop layer's own input stage");
        let ratio = f64::from(rect.width) / f64::from(rect.height);
        assert!(
            (ratio - 1.5).abs() <= 2.0 / f64::from(rect.height),
            "original is the crop layer's own 480x320 input stage ratio, not {ratio} from {rect:?}"
        );
        // Effect identities the host uses to find the layers of a stack.
        assert_eq!(Layer::pixel(0, 0, [1, 2, 3]).effect_id, PIXEL_EFFECT);
    }
}
