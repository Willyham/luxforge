//! The Basic adjustment module: one colour-stage layer holding every Basic parameter, edited by
//! one field-patch action.
//!
//! This slice implements Temperature and Tint, Exposure, the five Tone controls (Contrast,
//! Highlights, Shadows, Whites and Blacks), Vibrance and Saturation. A payload is a JSON object
//! whose keys are the implemented parameter names; a missing key is neutral, so the canonical
//! neutral payload is the empty object `{}` and `{"exposure": 0}` is the same state written
//! differently. The field-patch behaviour every such module shares lives in
//! [`super::field_patch`]; this file is the field table, the compilation and the neutral picker.
//!
//! The units compile in the frozen internal order — white balance, then exposure, then the tonal
//! curve, then vibrance and saturation — whatever order the fields were set in, so the result never
//! depends on which slider a person touched first. Each unit's equations live in its own file.
//!
//! The host places the layer by its effect stage: a colour-stage commit joins the stack before the
//! geometry tail, like a pixel replacement, and stays at that position for the rest of its life.
//! Later sets update it in place at the same identity and index.
//!
//! The module also answers one read-only query, `neutral-sample`: the neutral picker, which reads a
//! bounded patch of the stage this layer receives and solves the white balance that makes it
//! neutral. It commits nothing.
/// The Oklab conversion the two colour modules share. `pub(crate)` so the mixer's unit reuses
/// the same matrices and helpers instead of restating them; nothing in it changed when it was
/// opened up.
pub(crate) mod colour;
mod exposure;
/// `pub(crate)` rather than private: the vignette module's positive-amount branch reuses this
/// unit's `encode_srgb_extended`/`decode_srgb_extended` rather than duplicating the sRGB OETF a
/// third time in production (see `docs/design/vignette-study.md`'s "Amount"). No equation in this
/// file changes for that reuse; only this module's own visibility does.
pub(crate) mod tone;
mod white_balance;

use super::{
    ActionDescriptor, CanvasInteraction, ColorOperation, Control, EffectDescriptor, EffectStage,
    ParameterDescriptor, ParameterKind, PointwiseColor, Processing, Stage, StageContext,
    field_patch::{ActionText, Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::{EFFECT_FORMAT, Error, ErrorKind};
use colour::ColourAdjust;
use exposure::Exposure;
use serde_json::{Map, Value};
use std::sync::Arc;
use tone::Tone;
use white_balance::{PARAMETER_RANGE, WhiteBalance};

/// The one colour-stage effect of the Basic module: every implemented Basic parameter of a stack
/// lives in one layer of this effect.
pub const BASIC_EFFECT: &str = "luxforge.basic.adjust";

pub(super) const SET_BASIC: &str = "set-basic";
pub(super) const RESET_BASIC: &str = "reset-basic";
/// The read-only query the neutral picker runs, in its own `query.<id>` namespace.
pub(super) const NEUTRAL_SAMPLE: &str = "neutral-sample";

const EXPOSURE: &str = "exposure";
const TEMPERATURE: &str = "temperature";
const TINT: &str = "tint";
/// The five Tone-curve fields, each in the agreed -100..100 UI range with step 1 and no display
/// decimals, holding no unit.
const CONTRAST: &str = "contrast";
const HIGHLIGHTS: &str = "highlights";
const SHADOWS: &str = "shadows";
const WHITES: &str = "whites";
const BLACKS: &str = "blacks";
/// Vibrance and Saturation: `docs/design/basic-colour.md`'s frozen range, step and precision. No
/// unit is declared; the design states the accepted range directly in slider units.
const VIBRANCE: &str = "vibrance";
const SATURATION: &str = "saturation";

/// The neutral picker's name, on its control in the White balance group and on its canvas mode.
const NEUTRAL_PICKER_LABEL: &str = "Neutral picker";

/// Every implemented Basic field, in the payload's declared order. A later slice adds further
/// optional keys of the same format, and a neutral-defaulting key changes no existing
/// interpretation.
#[cfg(test)]
const FIELDS: [&str; 10] = [
    TEMPERATURE,
    TINT,
    EXPOSURE,
    CONTRAST,
    HIGHLIGHTS,
    SHADOWS,
    WHITES,
    BLACKS,
    VIBRANCE,
    SATURATION,
];

/// The neutral value of every Basic field.
const NEUTRAL: f64 = 0.0;

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check.
#[cfg(test)]
pub(crate) const AMBIGUOUS: &str = "ambiguous Basic layers";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// Temperature and Tint share a range, a step and a precision; only their name, label and the
/// direction they describe differ.
fn white_balance(name: &'static str, label: &str, notes: &str) -> Field {
    Field {
        min: -PARAMETER_RANGE,
        max: PARAMETER_RANGE,
        ..Field::slider(name, label, notes)
    }
}

/// The neutral picker's coordinates, in the content stage the Basic layer's input addresses. The
/// declared bound is the host's own maximum side, because a query's descriptor cannot know the
/// stage a particular asset produces; a point outside the actual stage is refused when it is asked.
fn sample_coordinate(name: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Integer {
            min: 0,
            max: MAX_COORDINATE,
        },
        required: true,
        default: None,
        unit: Some("px".into()),
        step: None,
        precision: None,
        notes: format!(
            "the {name} coordinate, in pixels of the stage the Basic layer receives: the content \
             stage, the source after EXIF orientation plus any pixel replacement before it. Map a \
             rendered pixel to it with render.locate"
        ),
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
    }
}

/// The largest coordinate a query accepts, matching the host's maximum image side.
const MAX_COORDINATE: i64 = 16383;

/// The Basic module's table, compilation and neutral picker.
#[derive(Debug, Default)]
pub struct Basic;

/// The Basic module: `Basic` as a field-patch module.
pub type BasicModule = FieldPatchModule<Basic>;

impl FieldPatch for Basic {
    fn spec() -> Spec {
        let tone = |name, label, notes| Field::slider(name, label, notes);
        Spec {
            id: "luxforge.basic",
            title: "Basic",
            hint: "Exposure, tone, white balance and colour",
            noun: "basic",
            effect: EffectDescriptor {
                id: BASIC_EFFECT.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Color,
                order: 0,
                maskable: true,
                artifacts: false,
                single: true,
            },
            set: ActionText {
                id: SET_BASIC,
                title: "Set Basic",
                notes: "merges the named Basic fields into the stack's one Basic layer, which the host places before the geometry tail on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op",
            },
            reset: ActionText {
                id: RESET_BASIC,
                title: "Reset Basic",
                notes: "returns the stack's one Basic layer to its neutral payload, keeping its identity and position; a no-op without one and when it is already neutral",
            },
            fields: vec![
                Field {
                    rail: Some(crate::RailDecoration::Temperature),
                    ..white_balance(
                        TEMPERATURE,
                        "Temperature",
                        "a relative warm/cool correction of the rendered JPEG, not a camera Kelvin value: 0 is the image's existing rendering and nothing here recovers or reproduces the camera's own white balance. Positive temperature warms the image, raising red and lowering blue; negative cools it. The correction is a von Kries chromatic adaptation in Bradford LMS anchored at the sRGB D65 white",
                    )
                },
                Field {
                    rail: Some(crate::RailDecoration::Tint),
                    ..white_balance(
                        TINT,
                        "Tint",
                        "a relative green/magenta correction of the rendered JPEG, not a camera Kelvin or tint value: 0 is the image's existing rendering. Positive tint is magenta, raising red and blue and lowering green; negative is green. It offsets the target chromaticity perpendicular to the daylight locus in CIE 1960 (u, v)",
                    )
                },
                Field {
                    min: -5.0,
                    max: 5.0,
                    step: 0.01,
                    precision: 2,
                    unit: Some("EV"),
                    ..Field::slider(
                        EXPOSURE,
                        "Exposure",
                        "multiplies the linear-light channels by 2^EV. The input is a rendered sRGB JPEG decoded through the sRGB transfer function, not scene-linear RAW data, so this is an exposure correction of a rendered image and cannot recover detail a clipped plateau no longer holds",
                    )
                },
                tone(
                    CONTRAST,
                    "Contrast",
                    "changes midtone separation with a fixed pivot at encoded mid-grey using a smooth, monotone S-curve",
                ),
                tone(
                    HIGHLIGHTS,
                    "Highlights",
                    "smoothly lifts or crushes the image's bright tones while leaving pure white exactly unchanged",
                ),
                tone(
                    SHADOWS,
                    "Shadows",
                    "smoothly lifts or crushes the image's dark tones while leaving pure black exactly unchanged",
                ),
                tone(
                    WHITES,
                    "Whites",
                    "moves the white point, extending or protecting highlight clipping, separately from Highlights",
                ),
                tone(
                    BLACKS,
                    "Blacks",
                    "moves the black point, crushing or lifting the darkest tones, separately from Shadows",
                ),
                Field::slider(
                    VIBRANCE,
                    "Vibrance",
                    "raises chroma more for near-neutral colour than for colour already close to the sRGB gamut edge, with reduced gain in a skin-like hue band; that hue weighting is a colour heuristic, not skin detection, and is not a promise about every skin tone",
                ),
                Field::slider(
                    SATURATION,
                    "Saturation",
                    "scales chroma uniformly about the achromatic axis; -100 is neutral grayscale, not merely a strong desaturation",
                ),
            ],
            groups: vec![
                Group {
                    label: "White balance",
                    fields: vec![TEMPERATURE, TINT],
                    collapsed: false,
                    // The neutral picker, beside the two fields a pick sets.
                    extra: vec![Control::Picker {
                        label: NEUTRAL_PICKER_LABEL.into(),
                    }],
                },
                Group {
                    label: "Tone",
                    fields: vec![EXPOSURE, CONTRAST, HIGHLIGHTS, SHADOWS, WHITES, BLACKS],
                    collapsed: false,
                    extra: Vec::new(),
                },
                Group {
                    label: "Colour",
                    fields: vec![VIBRANCE, SATURATION],
                    collapsed: false,
                    extra: Vec::new(),
                },
            ],
            queries: vec![ActionDescriptor {
                id: NEUTRAL_SAMPLE.into(),
                title: "Neutral sample".into(),
                notes: "reads a 5x5 patch of the stage the Basic layer receives, centred on the named content pixel and clipped at that stage's edges, and returns the temperature and tint that make its average neutral. It evaluates before the Basic layer, so picking the same patch twice gives the same answer whatever white balance is already set. A clipped, near-black or non-finite patch, a correction outside the representable range and a point outside the stage are each refused with their reason; nothing is guessed, clamped or committed".into(),
                summary: None,
                patch: false,
                parameters: vec![sample_coordinate("x"), sample_coordinate("y")],
            }],
            // The neutral picker: a pick runs the query at the content pixel behind it and submits
            // the settings it returns to `set-basic` once. A refusal commits nothing.
            canvas: Some(CanvasInteraction::SampleApply {
                query: NEUTRAL_SAMPLE.into(),
                x: "x".into(),
                y: "y".into(),
                action: SET_BASIC.into(),
                title: NEUTRAL_PICKER_LABEL.into(),
                shortcut: Some("W".into()),
            }),
            collapsed: false,
            layout: crate::ModuleLayout::Stacked,
        }
    }

    fn compile(&self, values: &Values<'_>, _: Stage) -> Result<Processing, Error> {
        // A neutral payload compiles to no units, which the host drops entirely: the identity byte
        // path and the shared source buffer are kept.
        if values.all_default() {
            return Ok(Processing::Color(ColorOperation::neutral()));
        }
        // The frozen internal order: white balance, then exposure, then the tonal curve, then
        // vibrance and saturation. Each unit is added only when its own field is not neutral, so a
        // layer that moves one slider costs one unit.
        let mut units: Vec<Arc<dyn PointwiseColor>> = Vec::new();
        let temperature = values.get(TEMPERATURE);
        let tint = values.get(TINT);
        if temperature != NEUTRAL || tint != NEUTRAL {
            units.push(Arc::new(WhiteBalance::new(temperature, tint)));
        }
        let exposure = values.get(EXPOSURE);
        if exposure != NEUTRAL {
            units.push(Arc::new(Exposure::new(exposure)));
        }
        let contrast = values.get(CONTRAST);
        let highlights = values.get(HIGHLIGHTS);
        let shadows = values.get(SHADOWS);
        let whites = values.get(WHITES);
        let blacks = values.get(BLACKS);
        if [contrast, highlights, shadows, whites, blacks] != [NEUTRAL; 5] {
            units.push(Arc::new(Tone::new(
                contrast, highlights, shadows, whites, blacks,
            )));
        }
        // Colour runs last: vibrance and saturation compile into one fused `ColourAdjust` unit
        // (see `colour.rs`) whenever at least one of the two fields is non-neutral, so a layer
        // that moves either slider alone still costs exactly one unit, and moving both costs one
        // unit rather than two.
        let vibrance = values.get(VIBRANCE);
        let saturation = values.get(SATURATION);
        if vibrance != NEUTRAL || saturation != NEUTRAL {
            units.push(Arc::new(ColourAdjust::new(vibrance, saturation)));
        }
        Ok(Processing::Color(ColorOperation::new(units)))
    }

    /// The neutral picker, the one query this module declares.
    ///
    /// The patch is read from the stage the Basic layer receives — the stage at that layer's index,
    /// or at the index a first commit would take when no layer exists — so a pick sees the image
    /// before this module's own correction and picking the same patch twice gives the same answer
    /// whatever is already set. Every sample is a point query through the host's compiled
    /// evaluation, so at most 25 points are evaluated at `O(layers)` each and no frame is
    /// allocated.
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if query_id != NEUTRAL_SAMPLE {
            return Err(validation(format!("unknown query {query_id}")));
        }
        let coordinate = |name: &str| -> Result<i64, Error> {
            parameters
                .get(name)
                .and_then(Value::as_i64)
                .ok_or_else(|| validation(format!("neutral sample needs an integer {name}")))
        };
        let (centre_x, centre_y) = (coordinate("x")?, coordinate("y")?);

        let index = match context.own_layer(BASIC_EFFECT)? {
            Some((index, _)) => index,
            // The stage this module's own layer would be committed at, by its declared stage and
            // order, so the picker reads the pixels the layer it creates will receive.
            None => context.insertion_index_for(BASIC_EFFECT),
        };
        let stage = context.stage_before(index)?;
        let outside = || {
            validation(format!(
                "outside the stage: ({centre_x}, {centre_y}) is not inside the {}x{} stage this \
                 Basic layer receives",
                stage.width, stage.height
            ))
        };
        if centre_x < 0
            || centre_y < 0
            || centre_x >= i64::from(stage.width)
            || centre_y >= i64::from(stage.height)
        {
            return Err(outside());
        }

        // Up to 5x5 pixel centres, clipped at the stage's edges: a position outside the stage is
        // dropped rather than clamped or wrapped, so a corner patch can be as small as one pixel
        // and is never empty for an in-bounds centre.
        let left = (centre_x - 2).max(0);
        let top = (centre_y - 2).max(0);
        let right = (centre_x + 2).min(i64::from(stage.width) - 1);
        let bottom = (centre_y + 2).min(i64::from(stage.height) - 1);
        let mut pixels: Vec<[u8; 3]> = Vec::with_capacity(25);
        for y in top..=bottom {
            for x in left..=right {
                let sampled = context.sample_before(index, x as u32, y as u32)?;
                let rgba = sampled.ok_or_else(outside)?;
                pixels.push([rgba[0], rgba[1], rgba[2]]);
            }
        }

        let mean =
            white_balance::average_patch(&pixels).map_err(|reason| validation(reason.message()))?;
        let (temperature, tint) =
            white_balance::neutral_settings(mean).map_err(|reason| validation(reason.message()))?;
        Ok(serde_json::json!({
            TEMPERATURE: temperature,
            TINT: tint,
            "patch": {
                "x": left,
                "y": top,
                "width": right - left + 1,
                "height": bottom - top + 1,
                "pixels": pixels,
                "mean_linear": mean,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{ActionInput, ActionPlan, ResetAction, ToolModule};
    use crate::{Layer, LayerId};
    use crate::{ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT, modules::check_parameters};
    use serde_json::json;

    const STAGE: Stage = Stage {
        width: 480,
        height: 320,
    };

    fn basic_layer(payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    /// Plan one request the way the host does: generic parameter check, module parse, then plan
    /// against a stack whose stage questions are answered from constants.
    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        planned_for(action, parameters, layers, None)
    }

    /// [`planned`] for one target: the global layer, or a mask's.
    fn planned_for(
        action: &str,
        parameters: Value,
        layers: &[Layer],
        target: Option<&crate::MaskId>,
    ) -> Result<ActionPlan, Error> {
        let module = BasicModule::new();
        let declared = module
            .descriptor()
            .action(action)
            .expect("a declared action");
        let checked = check_parameters(declared, &parameters)?;
        let input = module.parse(action, &checked)?;
        let stage = crate::modules::FixedStage::new(STAGE).reading([0, 0, 0, 255]);
        let registry = crate::ModuleRegistry::builtin();
        module.plan(
            &input,
            &StageContext {
                target,
                ..stage.context(layers, &registry)
            },
        )
    }

    fn committed(plan: ActionPlan) -> Layer {
        match plan {
            // The layer the host stores for the plan: a commit's effect and payload, or the
            // updated layer's identity with its new payload.
            ActionPlan::Commit(new) => Layer::new(new.effect_id, new.payload),
            ActionPlan::Update(update) => Layer {
                id: update.id,
                ..Layer::new(BASIC_EFFECT, update.payload)
            },
            ActionPlan::NoOp => panic!("expected a layer, not a no-op"),
            ActionPlan::Compose(_) => panic!("expected a layer, not a composite"),
            ActionPlan::Edits(_) => panic!("expected a layer, not several edits"),
        }
    }

    #[test]
    fn the_descriptor_declares_one_colour_effect_two_actions_and_the_tone_group() {
        let module = BasicModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "luxforge.basic");
        assert_eq!(descriptor.title, "Basic");
        assert_eq!(
            descriptor.hint.as_deref(),
            Some("Exposure, tone, white balance and colour")
        );
        assert!(!descriptor.developer);
        assert_eq!(descriptor.effects.len(), 1);
        assert_eq!(descriptor.effects[0].id, BASIC_EFFECT);
        assert_eq!(descriptor.effects[0].format, 1);
        assert_eq!(descriptor.effects[0].stage, EffectStage::Color);
        assert_eq!(
            descriptor.reset,
            Some(ResetAction {
                action: RESET_BASIC.into(),
                preset: Map::new(),
            })
        );

        let set = descriptor.action(SET_BASIC).expect("set-basic");
        assert!(set.patch, "every slider sends one field");
        assert!(set.summary.is_none());
        assert_eq!(set.parameters.len(), 10);
        assert_eq!(
            set.parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>(),
            FIELDS,
            "every implemented field is a declared parameter of the one patch action, in FIELDS \
             order"
        );
        let exposure = set.parameter(EXPOSURE).expect("the exposure parameter");
        assert_eq!(
            exposure.kind,
            ParameterKind::Number {
                min: -5.0,
                max: 5.0
            }
        );
        assert!(!exposure.required);
        assert_eq!(exposure.default, Some(json!(0.0)));
        assert_eq!(exposure.unit.as_deref(), Some("EV"));
        assert_eq!(exposure.step, Some(0.01));
        assert_eq!(exposure.precision, Some(2));
        assert!(
            exposure.notes.contains("2^EV") && exposure.notes.contains("not scene-linear RAW"),
            "{}",
            exposure.notes
        );

        for (name, note_needle) in [
            (CONTRAST, ""),
            (HIGHLIGHTS, ""),
            (SHADOWS, ""),
            (WHITES, ""),
            (BLACKS, ""),
            (VIBRANCE, "colour heuristic"),
            (SATURATION, "neutral grayscale"),
        ] {
            let parameter = set.parameter(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                parameter.kind,
                ParameterKind::Number {
                    min: -100.0,
                    max: 100.0
                },
                "{name}"
            );
            assert!(!parameter.required, "{name}");
            assert_eq!(parameter.unit, None, "{name}: no unit is declared");
            assert_eq!(parameter.step, Some(1.0), "{name}");
            assert_eq!(parameter.precision, Some(0), "{name}");
            assert!(!parameter.notes.is_empty(), "{name}");
            assert!(
                parameter.notes.contains(note_needle),
                "{name}: {}",
                parameter.notes
            );
        }

        let reset = descriptor.action(RESET_BASIC).expect("reset-basic");
        assert!(reset.parameters.is_empty());
        assert!(!reset.patch);

        assert_eq!(
            descriptor.controls,
            vec![
                Control::Group {
                    label: "White balance".into(),
                    reset: Some(ResetAction {
                        action: SET_BASIC.into(),
                        preset: [
                            ("temperature".to_owned(), json!(0.0)),
                            ("tint".to_owned(), json!(0.0)),
                        ]
                        .into_iter()
                        .collect(),
                    }),
                    controls: vec![
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "temperature".into(),
                            label: "Temperature".into(),
                            style: crate::NumberStyle::Slider,
                            rail: Some(crate::RailDecoration::Temperature),
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "tint".into(),
                            label: "Tint".into(),
                            style: crate::NumberStyle::Slider,
                            rail: Some(crate::RailDecoration::Tint),
                            reset: None,
                        },
                        Control::Picker {
                            label: "Neutral picker".into(),
                        },
                    ],
                    collapsed: false,
                },
                Control::Group {
                    label: "Tone".into(),
                    reset: Some(ResetAction {
                        action: SET_BASIC.into(),
                        preset: [
                            ("exposure".to_owned(), json!(0.0)),
                            ("contrast".to_owned(), json!(0.0)),
                            ("highlights".to_owned(), json!(0.0)),
                            ("shadows".to_owned(), json!(0.0)),
                            ("whites".to_owned(), json!(0.0)),
                            ("blacks".to_owned(), json!(0.0)),
                        ]
                        .into_iter()
                        .collect(),
                    }),
                    controls: vec![
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "exposure".into(),
                            label: "Exposure".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "contrast".into(),
                            label: "Contrast".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "highlights".into(),
                            label: "Highlights".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "shadows".into(),
                            label: "Shadows".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "whites".into(),
                            label: "Whites".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "blacks".into(),
                            label: "Blacks".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                    ],
                    collapsed: false,
                },
                Control::Group {
                    label: "Colour".into(),
                    reset: Some(ResetAction {
                        action: SET_BASIC.into(),
                        preset: [
                            ("vibrance".to_owned(), json!(0.0)),
                            ("saturation".to_owned(), json!(0.0)),
                        ]
                        .into_iter()
                        .collect(),
                    }),
                    controls: vec![
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "vibrance".into(),
                            label: "Vibrance".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "saturation".into(),
                            label: "Saturation".into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                            reset: None,
                        },
                    ],
                    collapsed: false,
                },
            ],
            "the White balance group's two sliders and its neutral picker, then the Tone group's \
             six, then the Colour group's two, each with a group reset naming all of its fields"
        );
    }

    /// The White balance group, its two sliders, its own reset preset and the neutral picker it
    /// puts on the canvas mode strip.
    #[test]
    fn the_descriptor_declares_the_white_balance_group_and_the_neutral_picker() {
        let module = BasicModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(
            descriptor.controls.first(),
            Some(&Control::Group {
                label: "White balance".into(),
                reset: Some(ResetAction {
                    action: SET_BASIC.into(),
                    preset: [
                        ("temperature".to_owned(), json!(0.0)),
                        ("tint".to_owned(), json!(0.0)),
                    ]
                    .into_iter()
                    .collect(),
                }),
                controls: vec![
                    Control::Number {
                        action: SET_BASIC.into(),
                        parameter: "temperature".into(),
                        label: "Temperature".into(),
                        style: crate::NumberStyle::Slider,
                        rail: Some(crate::RailDecoration::Temperature),
                        reset: None,
                    },
                    Control::Number {
                        action: SET_BASIC.into(),
                        parameter: "tint".into(),
                        label: "Tint".into(),
                        style: crate::NumberStyle::Slider,
                        rail: Some(crate::RailDecoration::Tint),
                        reset: None,
                    },
                    Control::Picker {
                        label: "Neutral picker".into(),
                    },
                ],
                collapsed: false,
            }),
            "white balance is the first group, before tone, and the neutral picker sits with the \
             two fields a pick sets"
        );

        let set = descriptor.action(SET_BASIC).expect("set-basic");
        for (name, expected_label) in [(TEMPERATURE, "warms"), (TINT, "magenta")] {
            let parameter = set.parameter(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                parameter.kind,
                ParameterKind::Number {
                    min: -100.0,
                    max: 100.0
                },
                "{name}"
            );
            assert!(!parameter.required, "{name}");
            assert_eq!(parameter.default, Some(json!(0.0)), "{name}");
            assert_eq!(parameter.unit, None, "{name}: not Kelvin, not a unit");
            assert_eq!(parameter.step, Some(1.0), "{name}");
            assert_eq!(parameter.precision, Some(0), "{name}");
            assert!(
                parameter.notes.contains("relative")
                    && parameter.notes.contains("rendered JPEG")
                    && parameter.notes.contains(expected_label),
                "{name}: {}",
                parameter.notes
            );
        }

        let query = descriptor.query(NEUTRAL_SAMPLE).expect("neutral-sample");
        assert_eq!(query.title, "Neutral sample");
        assert!(!query.patch);
        assert_eq!(
            query
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>(),
            ["x", "y"]
        );
        for parameter in &query.parameters {
            assert_eq!(
                parameter.kind,
                ParameterKind::Integer { min: 0, max: 16383 }
            );
            assert!(parameter.required);
            assert!(
                parameter.notes.contains("render.locate"),
                "{}",
                parameter.notes
            );
        }
        assert_eq!(
            descriptor.canvas,
            Some(CanvasInteraction::SampleApply {
                query: NEUTRAL_SAMPLE.into(),
                x: "x".into(),
                y: "y".into(),
                action: SET_BASIC.into(),
                title: "Neutral picker".into(),
                shortcut: Some("W".into()),
            })
        );
        assert_eq!(
            descriptor.canvas.as_ref().unwrap().title(),
            "Neutral picker"
        );
        assert_eq!(descriptor.canvas.as_ref().unwrap().shortcut(), Some("W"));
    }

    #[test]
    fn a_payload_is_refused_by_shape_format_field_and_range_without_being_rewritten() {
        let module = BasicModule::new();
        let refused = |format: u32, payload: Value| {
            module
                .validate_payload(BASIC_EFFECT, format, &payload)
                .expect_err("an invalid payload")
        };
        for payload in [
            json!({}),
            json!({"exposure": 0}),
            json!({"exposure": -5.0}),
            json!({"exposure": 5.0}),
            json!({"vibrance": -100.0}),
            json!({"vibrance": 100.0}),
            json!({"saturation": -100.0}),
            json!({"saturation": 100.0}),
            json!({"vibrance": 50.0, "saturation": -20.0}),
        ] {
            module
                .validate_payload(BASIC_EFFECT, 1, &payload)
                .unwrap_or_else(|error| panic!("{payload} should be valid: {error}"));
        }

        let format = refused(2, json!({"exposure": 1.0}));
        assert_eq!(format.kind, ErrorKind::Incompatible);
        assert!(format.detail.contains("unsupported effect format 2"));

        let effect = module
            .validate_payload("luxforge.other", 1, &json!({}))
            .expect_err("another effect");
        assert_eq!(effect.kind, ErrorKind::Incompatible);

        for (case, payload, needle) in [
            ("a list", json!([1.0]), "must be a JSON object"),
            ("a number", json!(1.0), "must be a JSON object"),
            (
                "an unknown key",
                json!({"gamma": 10.0}),
                "unknown basic field gamma",
            ),
            (
                "a string value",
                json!({"exposure": "1.0"}),
                "must be a finite number",
            ),
            (
                "below the range",
                json!({"exposure": -5.001}),
                "within -5..=5",
            ),
            (
                "above the range",
                json!({"exposure": 5.001}),
                "within -5..=5",
            ),
        ] {
            let error = refused(1, payload);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(needle), "{case}: {}", error.detail);
        }
    }

    #[test]
    fn the_canonical_neutral_payload_is_the_empty_object_and_compares_equal_to_an_explicit_zero() {
        // First non-neutral set commits a layer holding only the field it set.
        let committed_layer = committed(planned(SET_BASIC, json!({"exposure": 0.5}), &[]).unwrap());
        assert_eq!(committed_layer.payload, json!({"exposure": 0.5}));
        assert_eq!(committed_layer.effect_id, BASIC_EFFECT);
        assert_eq!(committed_layer.effect_format, 1);

        // A set back to zero stores the canonical neutral form, keeping the layer.
        let neutralized = committed(
            planned(
                SET_BASIC,
                json!({"exposure": 0.0}),
                &[basic_layer(json!({"exposure": 0.5}))],
            )
            .unwrap(),
        );
        assert_eq!(neutralized.payload, json!({}));

        // Both spellings of neutral are the same state, in both directions.
        for stored in [json!({}), json!({"exposure": 0.0})] {
            assert_eq!(
                planned(
                    SET_BASIC,
                    json!({"exposure": 0.0}),
                    &[basic_layer(stored.clone())]
                )
                .unwrap(),
                ActionPlan::NoOp,
                "setting neutral on a {stored} layer changes nothing"
            );
            assert_eq!(
                planned(RESET_BASIC, json!({}), &[basic_layer(stored.clone())]).unwrap(),
                ActionPlan::NoOp,
                "resetting a {stored} layer changes nothing"
            );
            assert_eq!(
                BasicModule::new()
                    .describe_layer(BASIC_EFFECT, 1, &stored)
                    .unwrap(),
                "Neutral"
            );
        }
    }

    #[test]
    fn a_set_commits_updates_or_reports_a_no_op_against_the_stack_it_finds() {
        // No layer and a neutral result adds nothing.
        assert_eq!(
            planned(SET_BASIC, json!({"exposure": 0.0}), &[]).unwrap(),
            ActionPlan::NoOp
        );
        assert_eq!(
            planned(SET_BASIC, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "an empty patch sets nothing"
        );

        // An existing layer is updated in place, keeping its identity.
        let existing = basic_layer(json!({"exposure": -1.0}));
        let plan = planned(
            SET_BASIC,
            json!({"exposure": 2.25}),
            std::slice::from_ref(&existing),
        )
        .unwrap();
        match plan {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, existing.id, "the layer keeps its identity");
                assert_eq!(layer.payload, json!({"exposure": 2.25}));
            }
            other => panic!("expected an update, got {other:?}"),
        }

        // The same value again is a no-op, whichever way the number was written.
        for same in [json!({"exposure": -1.0}), json!({"exposure": -1})] {
            assert_eq!(
                planned(SET_BASIC, same, std::slice::from_ref(&existing)).unwrap(),
                ActionPlan::NoOp
            );
        }

        // A reset keeps the layer's identity and stores the neutral payload.
        match planned(RESET_BASIC, json!({}), std::slice::from_ref(&existing)).unwrap() {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, existing.id);
                assert_eq!(layer.payload, json!({}));
            }
            other => panic!("expected an update, got {other:?}"),
        }
        assert_eq!(
            planned(RESET_BASIC, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "a reset without a layer is a no-op"
        );
    }

    #[test]
    fn a_patch_merges_over_the_stored_payload_and_leaves_omitted_fields_alone() {
        // The generic check fills no default for a patch action, so an omitted field never arrives
        // as a neutral value that would silently clear it.
        let module = BasicModule::new();
        let declared = module.descriptor().action(SET_BASIC).expect("set-basic");
        assert_eq!(check_parameters(declared, &json!({})).unwrap(), Map::new());
        assert_eq!(
            check_parameters(declared, &json!({"exposure": 1.5}))
                .unwrap()
                .get(EXPOSURE),
            Some(&json!(1.5))
        );
        assert!(
            check_parameters(declared, &json!({"gamma": 1.0})).is_err(),
            "an unknown field is still rejected"
        );
        assert!(
            check_parameters(declared, &json!({"exposure": 6.0})).is_err(),
            "the declared range still applies"
        );

        // An empty patch over a stored value preserves it.
        let stored = basic_layer(json!({"exposure": 1.5}));
        assert_eq!(
            planned(SET_BASIC, json!({}), std::slice::from_ref(&stored)).unwrap(),
            ActionPlan::NoOp
        );
        // The request stores the patch as sent, not the merged payload.
        let input = module
            .parse(
                SET_BASIC,
                &check_parameters(declared, &json!({"exposure": 1.5})).unwrap(),
            )
            .unwrap();
        assert_eq!(input.action_id, SET_BASIC);
        assert_eq!(
            input.parameters,
            json!({"exposure": 1.5}).as_object().cloned().unwrap()
        );
        assert_eq!(
            module.parse(RESET_BASIC, &Map::new()).unwrap().parameters,
            Map::new()
        );
        assert!(module.parse("set-crop", &Map::new()).is_err());
    }

    /// A set addresses the layer of its own target through the host's one lookup, even over a stack
    /// that also holds the other target's layer: the global layer and each mask's are distinct.
    #[test]
    fn a_set_updates_the_layer_of_its_own_target() {
        let mask = crate::MaskId::new();
        let global = basic_layer(json!({"exposure": 1.0}));
        let masked = Layer {
            mask: Some(mask.clone()),
            ..basic_layer(json!({"exposure": -1.0}))
        };
        let stack = [global.clone(), masked.clone()];
        for (target, expected) in [(None, &global), (Some(&mask), &masked)] {
            match planned_for(SET_BASIC, json!({"exposure": 0.5}), &stack, target).unwrap() {
                ActionPlan::Update(update) => assert_eq!(update.id, expected.id, "{target:?}"),
                other => panic!("expected an update for {target:?}, got {other:?}"),
            }
        }
        // Another mask has no layer yet, so its first set commits one.
        let other = crate::MaskId::new();
        assert!(matches!(
            planned_for(SET_BASIC, json!({"exposure": 0.5}), &stack, Some(&other)).unwrap(),
            ActionPlan::Commit(_)
        ));
    }

    #[test]
    fn two_basic_layers_are_ambiguous_rather_than_silently_resolved() {
        let stack = [
            basic_layer(json!({"exposure": 1.0})),
            basic_layer(json!({"exposure": -1.0})),
        ];
        for action in [SET_BASIC, RESET_BASIC] {
            let error = planned(action, json!({}), &stack).expect_err("an ambiguous stack");
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(error.detail, "ambiguous Basic layers");
        }
        let registry = crate::ModuleRegistry::builtin();
        assert!(
            registry.effect_single(BASIC_EFFECT),
            "the host refuses to compile the same stack"
        );
        assert!(!registry.effect_single(PIXEL_EFFECT));
    }

    #[test]
    fn labels_name_the_moved_field_the_reset_group_and_the_module_reset() {
        let module = BasicModule::new();
        let label = |action: &str, parameters: Value| {
            module.label(&ActionInput {
                action_id: action.to_owned(),
                parameters: parameters.as_object().cloned().unwrap(),
            })
        };
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 0.5})).as_deref(),
            Some("Exposure +0.50 EV")
        );
        assert_eq!(
            label(SET_BASIC, json!({"exposure": -1.0})).as_deref(),
            Some("Exposure -1.00 EV")
        );
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 5.0})).as_deref(),
            Some("Exposure +5.00 EV")
        );
        assert_eq!(
            label(SET_BASIC, json!({"contrast": 20.0})).as_deref(),
            Some("Contrast +20")
        );
        assert_eq!(
            label(SET_BASIC, json!({"highlights": -100.0})).as_deref(),
            Some("Highlights -100")
        );
        assert_eq!(
            label(SET_BASIC, json!({"shadows": 100.0})).as_deref(),
            Some("Shadows +100")
        );
        assert_eq!(
            label(SET_BASIC, json!({"whites": -50.0})).as_deref(),
            Some("Whites -50")
        );
        assert_eq!(
            label(SET_BASIC, json!({"blacks": 50.0})).as_deref(),
            Some("Blacks +50")
        );
        // A single field set back to neutral is now just that field's own label: the Tone group
        // has six fields, so one field alone is no longer indistinguishable from the group reset.
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 0.0})).as_deref(),
            Some("Exposure +0.00 EV")
        );
        // The Tone group reset is a patch holding every one of its six fields at neutral, however
        // it was sent, e.g. from the group's own preset.
        assert_eq!(
            label(
                SET_BASIC,
                json!({
                    "exposure": 0.0,
                    "contrast": 0.0,
                    "highlights": 0.0,
                    "shadows": 0.0,
                    "whites": 0.0,
                    "blacks": 0.0,
                })
            )
            .as_deref(),
            Some("Reset Tone")
        );
        // A patch changing several fields that is not a declared group reset names the count.
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 0.5, "contrast": 20.0})).as_deref(),
            Some("Basic (2 fields)")
        );
        assert_eq!(
            label(
                SET_BASIC,
                json!({"contrast": 20.0, "highlights": -20.0, "shadows": 20.0})
            )
            .as_deref(),
            Some("Basic (3 fields)")
        );
        assert_eq!(
            label(SET_BASIC, json!({"vibrance": 30.0})).as_deref(),
            Some("Vibrance +30")
        );
        assert_eq!(
            label(SET_BASIC, json!({"saturation": -100.0})).as_deref(),
            Some("Saturation -100")
        );
        assert_eq!(
            label(SET_BASIC, json!({"vibrance": 0.0, "saturation": 0.0})).as_deref(),
            Some("Reset Colour"),
            "the Colour group's fields at neutral, together, is that group's reset"
        );
        assert_eq!(
            label(SET_BASIC, json!({"vibrance": 50.0, "saturation": 20.0})).as_deref(),
            Some("Basic (2 fields)"),
            "a mixed non-reset patch names how many fields it touched"
        );
        assert_eq!(
            label(SET_BASIC, json!({"temperature": 25.0})).as_deref(),
            Some("Temperature +25")
        );
        assert_eq!(
            label(SET_BASIC, json!({"tint": -15.0})).as_deref(),
            Some("Tint -15")
        );
        assert_eq!(
            label(SET_BASIC, json!({"temperature": 0.0, "tint": 0.0})).as_deref(),
            Some("Reset White balance"),
            "the White balance group's fields at neutral, together, is that group's reset"
        );
        assert_eq!(
            label(SET_BASIC, json!({"temperature": 20.0, "tint": -10.0})).as_deref(),
            Some("Basic (2 fields)"),
            "the same two fields away from neutral are a plain patch, not the group reset"
        );
        assert_eq!(
            label(RESET_BASIC, json!({})).as_deref(),
            Some("Reset Basic")
        );
        assert_eq!(label(SET_BASIC, json!({})), None);
    }

    #[test]
    fn a_layer_describes_and_reports_its_values() {
        let module = BasicModule::new();
        assert_eq!(
            module
                .describe_layer(BASIC_EFFECT, 1, &json!({"exposure": 0.5}))
                .unwrap(),
            "Exposure +0.50 EV"
        );
        assert_eq!(
            module
                .values(BASIC_EFFECT, 1, &json!({"exposure": 0.5}))
                .unwrap(),
            json!({
                "temperature": 0.0,
                "tint": 0.0,
                "exposure": 0.5,
                "contrast": 0.0,
                "highlights": 0.0,
                "shadows": 0.0,
                "whites": 0.0,
                "blacks": 0.0,
                "vibrance": 0.0,
                "saturation": 0.0,
            })
            .as_object()
            .cloned()
            .unwrap()
        );
        assert_eq!(
            module.values(BASIC_EFFECT, 1, &json!({})).unwrap(),
            json!({
                "temperature": 0.0,
                "tint": 0.0,
                "exposure": 0.0,
                "contrast": 0.0,
                "highlights": 0.0,
                "shadows": 0.0,
                "whites": 0.0,
                "blacks": 0.0,
                "vibrance": 0.0,
                "saturation": 0.0,
            })
            .as_object()
            .cloned()
            .unwrap(),
            "a neutral layer reports the neutral value of every implemented field"
        );
        assert_eq!(
            module
                .values(
                    BASIC_EFFECT,
                    1,
                    &json!({"vibrance": 50.0, "saturation": -20.0})
                )
                .unwrap(),
            json!({
                "temperature": 0.0,
                "tint": 0.0,
                "exposure": 0.0,
                "contrast": 0.0,
                "highlights": 0.0,
                "shadows": 0.0,
                "whites": 0.0,
                "blacks": 0.0,
                "vibrance": 50.0,
                "saturation": -20.0,
            })
            .as_object()
            .cloned()
            .unwrap()
        );
        assert_eq!(
            module
                .describe_layer(BASIC_EFFECT, 1, &json!({"contrast": 20.0, "blacks": -50.0}))
                .unwrap(),
            "Contrast +20, Blacks -50"
        );
        assert!(module.values(BASIC_EFFECT, 2, &json!({})).is_err());
        assert!(module.describe_layer(BASIC_EFFECT, 2, &json!({})).is_err());
    }

    #[test]
    fn compilation_produces_an_exposure_unit_a_tone_unit_both_or_neither() {
        let module = BasicModule::new();
        let compiled = |payload: Value| module.compile(BASIC_EFFECT, 1, &payload, STAGE).unwrap();
        match compiled(json!({})) {
            Processing::Color(operation) => {
                assert!(
                    operation.is_empty(),
                    "a neutral payload compiles to nothing"
                );
                assert_eq!(operation, ColorOperation::neutral());
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // Every Tone field at neutral, spelled out explicitly, still compiles to nothing: no Tone
        // unit is ever constructed for an all-neutral parameter set.
        match compiled(json!({
            "contrast": 0.0, "highlights": 0.0, "shadows": 0.0, "whites": 0.0, "blacks": 0.0,
        })) {
            Processing::Color(operation) => assert!(operation.is_empty()),
            other => panic!("expected a colour operation, got {other:?}"),
        }
        match compiled(json!({"exposure": 0.5})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(operation.units()[0].describe(), "exposure(+0.5)");
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // A single non-neutral Tone field is enough to compile a Tone unit, holding every field
        // (neutral ones included) at its stored value.
        match compiled(json!({"contrast": 20.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(
                    operation.units()[0].describe(),
                    "tone(contrast=+20, highlights=+0, shadows=+0, whites=+0, blacks=+0)"
                );
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // A single non-neutral Colour field is enough to compile the fused Colour unit, holding
        // both fields (the neutral one included) at its stored value.
        match compiled(json!({"vibrance": 30.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(
                    operation.units()[0].describe(),
                    "colour-adjust(vibrance:+30, saturation:+0)"
                );
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        match compiled(json!({"saturation": -100.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(
                    operation.units()[0].describe(),
                    "colour-adjust(vibrance:+0, saturation:-100)"
                );
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // Both Colour fields non-neutral together still compile to the one fused unit.
        match compiled(json!({"vibrance": 30.0, "saturation": -100.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(
                    operation.units()[0].describe(),
                    "colour-adjust(vibrance:+30, saturation:-100)"
                );
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // Exposure and Tone together compile to two units, Exposure before Tone, matching the
        // internal order white balance -> exposure -> tone -> vibrance -> saturation.
        match compiled(json!({"exposure": 1.0, "shadows": 50.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 2);
                assert!(operation.is_finite());
                assert_eq!(operation.units()[0].describe(), "exposure(+1)");
                assert!(operation.units()[1].describe().starts_with("tone("));
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // One non-neutral white balance field compiles the one white-balance unit, holding both.
        match compiled(json!({"temperature": 20.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(operation.units()[0].describe(), "white-balance(+20, +0)");
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // The frozen internal order over every implemented field: white balance, exposure, tone,
        // then the fused Colour unit, whichever fields the payload set. Vibrance and saturation
        // together still cost one unit, not two.
        match compiled(json!({
            "temperature": 20.0,
            "tint": -5.0,
            "exposure": 1.0,
            "contrast": 20.0,
            "vibrance": 20.0,
            "saturation": 10.0,
        })) {
            Processing::Color(operation) => {
                let described = operation
                    .units()
                    .iter()
                    .map(|unit| unit.describe())
                    .collect::<Vec<_>>();
                assert_eq!(described.len(), 4);
                assert_eq!(described[0], "white-balance(+20, -5)");
                assert_eq!(described[1], "exposure(+1)");
                assert!(described[2].starts_with("tone("));
                assert_eq!(described[3], "colour-adjust(vibrance:+20, saturation:+10)");
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        assert!(
            module.compile(BASIC_EFFECT, 2, &json!({}), STAGE).is_err(),
            "an unsupported format never compiles"
        );
        assert!(
            module
                .compile(BASIC_EFFECT, 1, &json!({"exposure": 99.0}), STAGE)
                .is_err(),
            "a stored value outside the declared range never compiles"
        );
        assert!(
            module
                .compile(BASIC_EFFECT, 1, &json!({"contrast": 999.0}), STAGE)
                .is_err(),
            "a stored Tone value outside the declared range never compiles"
        );
        assert!(
            module
                .compile(BASIC_EFFECT, 1, &json!({"vibrance": 101.0}), STAGE)
                .is_err(),
            "a vibrance value outside the declared range never compiles"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The neutral picker query
    // -----------------------------------------------------------------------------------------

    /// A synthetic input stage the query samples, recording which layer index each sample asked
    /// about so a test can prove the patch was read *before* the Basic layer rather than after it.
    struct Probe {
        width: u32,
        height: u32,
        pixels: Vec<[u8; 3]>,
        asked: std::cell::RefCell<Vec<(usize, u32, u32)>>,
    }

    impl Probe {
        /// A stage filled with `background`, with `patch` written as a 5x5 block whose top-left
        /// corner is `(at_x, at_y)`.
        fn with_patch(
            width: u32,
            height: u32,
            background: [u8; 3],
            at_x: u32,
            at_y: u32,
            patch: &[[u8; 3]],
        ) -> Self {
            let mut pixels = vec![background; (width * height) as usize];
            for (index, pixel) in patch.iter().enumerate() {
                let (x, y) = (at_x + index as u32 % 5, at_y + index as u32 / 5);
                if x < width && y < height {
                    pixels[(y * width + x) as usize] = *pixel;
                }
            }
            Self {
                width,
                height,
                pixels,
                asked: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn uniform(width: u32, height: u32, colour: [u8; 3]) -> Self {
            Self::with_patch(width, height, colour, 0, 0, &[])
        }
    }

    /// Run the neutral picker the way the host does: generic parameter check, then the module's
    /// query against a stack whose stage questions the probe answers.
    fn queried(probe: &Probe, layers: &[Layer], x: i64, y: i64) -> Result<Value, Error> {
        let module = BasicModule::new();
        let declared = module
            .descriptor()
            .query(NEUTRAL_SAMPLE)
            .expect("a declared query");
        let checked = check_parameters(declared, &json!({"x": x, "y": y}))?;
        module.query(
            NEUTRAL_SAMPLE,
            &checked,
            &StageContext {
                stage: probe.stage(),
                layers,
                registry: &crate::ModuleRegistry::builtin(),
                target: None,
                questions: probe,
            },
        )
    }

    impl Probe {
        fn stage(&self) -> Stage {
            Stage {
                width: self.width,
                height: self.height,
            }
        }
    }

    /// Every prefix receives the probe's stage, and every point reads the probe's pixel there.
    impl crate::modules::StageQuestions for Probe {
        fn stage_before(&self, _: usize) -> Result<Stage, Error> {
            Ok(self.stage())
        }
        fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
            self.asked.borrow_mut().push((index, x, y));
            Ok((x < self.width && y < self.height).then(|| {
                let pixel = self.pixels[(y * self.width + x) as usize];
                [pixel[0], pixel[1], pixel[2], 255]
            }))
        }
    }

    #[derive(serde::Deserialize)]
    struct SolverCase {
        description: String,
        patch_u8: Vec<[u8; 3]>,
        expected_solution: Option<[i64; 2]>,
    }

    /// The committed solver corpus, so the query is tied to the same frozen numbers the unit is.
    fn solver_cases() -> Vec<SolverCase> {
        #[derive(serde::Deserialize)]
        struct Cases {
            solver_cases: Vec<SolverCase>,
        }
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/basic/white-balance-cases.json"
        ))
        .expect("the committed fixture");
        serde_json::from_str::<Cases>(&raw)
            .expect("white-balance cases")
            .solver_cases
    }

    /// A full 5x5 patch from the frozen corpus, read through the query, returns exactly the
    /// settings that corpus predicts, plus the patch rectangle and mean it averaged.
    #[test]
    fn the_query_returns_the_settings_the_solver_corpus_predicts() {
        let mut covered = 0;
        for case in solver_cases() {
            let (Some(expected), 25) = (case.expected_solution, case.patch_u8.len()) else {
                continue;
            };
            covered += 1;
            // The patch sits away from every edge, so all 25 samples are in bounds.
            let probe = Probe::with_patch(11, 11, [128, 128, 128], 3, 3, &case.patch_u8);
            let result =
                queried(&probe, &[], 5, 5).unwrap_or_else(|e| panic!("{}: {e}", case.description));
            assert_eq!(
                (result["temperature"].as_i64(), result["tint"].as_i64()),
                (Some(expected[0]), Some(expected[1])),
                "{}",
                case.description
            );
            assert_eq!(
                result["patch"],
                json!({
                    "x": 3, "y": 3, "width": 5, "height": 5,
                    "pixels": case.patch_u8,
                    "mean_linear": result["patch"]["mean_linear"],
                }),
                "{}",
                case.description
            );
            let mean = result["patch"]["mean_linear"]
                .as_array()
                .expect("three linear channels");
            assert_eq!(mean.len(), 3);
            assert!(
                mean.iter()
                    .all(|value| value.as_f64().is_some_and(f64::is_finite))
            );
        }
        assert_eq!(covered, 121, "every full-patch round-trip case of the grid");
    }

    /// The picker reads the stage the Basic layer *receives*: the layer's own index when one
    /// exists, and the index a first commit would take when none does. A pick therefore sees the
    /// image before this module's correction, however strong that correction already is.
    #[test]
    fn the_patch_is_sampled_before_the_basic_layer() {
        let case = solver_cases()
            .into_iter()
            .find(|case| case.expected_solution == Some([20, -20]))
            .expect("a grid case");
        let patch = case.patch_u8;
        let probe = Probe::with_patch(11, 11, [128, 128, 128], 3, 3, &patch);
        let without = queried(&probe, &[], 5, 5).expect("a solved patch");
        assert!(
            probe.asked.borrow().iter().all(|(index, _, _)| *index == 0),
            "with no Basic layer the picker samples the insertion index"
        );

        // The same stack with a strong white balance already applied. The picker asks about the
        // Basic layer's own index, so it reads the same stage and returns the same answer.
        let strong = basic_layer(json!({"temperature": 80.0, "tint": -40.0}));
        let after = Probe::with_patch(11, 11, [128, 128, 128], 3, 3, &patch);
        let with = queried(&after, std::slice::from_ref(&strong), 5, 5).expect("a solved patch");
        assert_eq!(
            after
                .asked
                .borrow()
                .iter()
                .map(|(index, _, _)| *index)
                .collect::<std::collections::BTreeSet<_>>(),
            [0].into_iter().collect(),
            "the Basic layer sits at index 0, so its input stage is the prefix before it"
        );
        assert_eq!(with, without, "the layer's own correction is not sampled");

        // A pixel replacement before the Basic layer *is* part of that input stage, so the picker
        // asks about the Basic layer's index, not about the content stage's index 0.
        let stack = [Layer::pixel(0, 0, [1, 2, 3]), strong];
        let later = Probe::with_patch(11, 11, [128, 128, 128], 3, 3, &patch);
        assert_eq!(
            queried(&later, &stack, 5, 5).expect("a solved patch"),
            without
        );
        assert!(
            later.asked.borrow().iter().all(|(index, _, _)| *index == 1),
            "the stage at the Basic layer's index carries the replacement before it"
        );
    }

    /// The patch is clipped at the stage's edges rather than clamped or wrapped: a corner pick
    /// averages the pixels that exist and says which rectangle it read.
    #[test]
    fn the_patch_is_clipped_at_the_stage_edges() {
        let grey = [150, 150, 150];
        for (case, (x, y), expected) in [
            ("the top-left corner", (0, 0), (0, 0, 3, 3)),
            ("one in from the corner", (1, 1), (0, 0, 4, 4)),
            ("the far corner", (7, 7), (5, 5, 3, 3)),
            ("the middle", (4, 4), (2, 2, 5, 5)),
            ("a left edge", (0, 4), (0, 2, 3, 5)),
        ] {
            let probe = Probe::uniform(8, 8, grey);
            let result = queried(&probe, &[], x, y).unwrap_or_else(|e| panic!("{case}: {e}"));
            let patch = &result["patch"];
            assert_eq!(
                (
                    patch["x"].as_i64().unwrap(),
                    patch["y"].as_i64().unwrap(),
                    patch["width"].as_i64().unwrap(),
                    patch["height"].as_i64().unwrap(),
                ),
                (expected.0, expected.1, expected.2, expected.3),
                "{case}"
            );
            assert_eq!(
                patch["pixels"].as_array().unwrap().len() as i64,
                expected.2 * expected.3,
                "{case}: every sampled pixel is reported"
            );
            assert!(
                probe
                    .asked
                    .borrow()
                    .iter()
                    .all(|(_, sx, sy)| *sx < 8 && *sy < 8),
                "{case}: no sample outside the stage"
            );
            // A neutral grey needs no correction at all.
            assert_eq!(
                (result["temperature"].as_i64(), result["tint"].as_i64()),
                (Some(0), Some(0)),
                "{case}"
            );
        }
    }

    /// Every refusal is structured, names its reason first and commits nothing.
    #[test]
    fn a_clipped_dark_or_out_of_stage_pick_is_refused_with_its_reason() {
        for (case, probe, x, y, prefix) in [
            (
                "a blown highlight in the patch",
                Probe::with_patch(11, 11, [200, 200, 200], 3, 3, &[[255, 250, 250]]),
                5,
                5,
                "clipped:",
            ),
            (
                "a crushed shadow in the patch",
                Probe::with_patch(11, 11, [200, 200, 200], 5, 5, &[[0, 4, 4]]),
                5,
                5,
                "clipped:",
            ),
            (
                "a near-black region",
                Probe::uniform(11, 11, [20, 20, 20]),
                5,
                5,
                "near-black:",
            ),
            (
                "a strongly saturated region",
                Probe::uniform(11, 11, [240, 60, 60]),
                5,
                5,
                "out-of-range:",
            ),
            (
                "a point past the right edge",
                Probe::uniform(8, 8, [150, 150, 150]),
                8,
                4,
                "outside the stage:",
            ),
            (
                "a point past the bottom edge",
                Probe::uniform(8, 8, [150, 150, 150]),
                4,
                100,
                "outside the stage:",
            ),
        ] {
            let error = queried(&probe, &[], x, y).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.starts_with(prefix), "{case}: {}", error.detail);
        }
        // A point outside the declared coordinate range never reaches the module at all.
        let probe = Probe::uniform(8, 8, [150, 150, 150]);
        assert!(queried(&probe, &[], -1, 0).is_err());
        assert!(queried(&probe, &[], 0, 99_999).is_err());
        assert!(
            probe.asked.borrow().is_empty(),
            "a refused request samples nothing"
        );
    }

    /// Two Basic layers are ambiguous for the picker exactly as they are for an action: it refuses
    /// to guess which layer's input stage a pick addresses, and rewrites nothing.
    #[test]
    fn the_query_refuses_an_ambiguous_stack_and_an_unknown_query() {
        let probe = Probe::uniform(11, 11, [150, 150, 150]);
        let stack = [
            basic_layer(json!({"temperature": 10.0})),
            basic_layer(json!({"tint": -10.0})),
        ];
        let error = queried(&probe, &stack, 5, 5).expect_err("an ambiguous stack");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(error.detail, AMBIGUOUS);

        let module = BasicModule::new();
        let error = module
            .query(
                "histogram",
                &json!({"x": 0, "y": 0}).as_object().cloned().unwrap(),
                &crate::modules::FixedStage::new(STAGE)
                    .reading([128, 128, 128, 255])
                    .context(&[], &crate::ModuleRegistry::builtin()),
            )
            .expect_err("an undeclared query");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("unknown query histogram"), "{error}");
    }

    /// Applying the settings a pick returns to that same patch neutralizes it: the three corrected
    /// channels land within one 8-bit code of each other.
    #[test]
    fn the_settings_a_pick_returns_neutralize_the_patch_it_read() {
        let module = BasicModule::new();
        for case in solver_cases() {
            let (Some(_), 25) = (case.expected_solution, case.patch_u8.len()) else {
                continue;
            };
            let probe = Probe::with_patch(11, 11, [128, 128, 128], 3, 3, &case.patch_u8);
            let result = queried(&probe, &[], 5, 5).expect("a solved patch");
            let payload = json!({
                "temperature": result["temperature"],
                "tint": result["tint"],
            });
            let Processing::Color(operation) = module
                .compile(BASIC_EFFECT, EFFECT_FORMAT, &payload, STAGE)
                .expect("a compiled correction")
            else {
                panic!("expected a colour operation");
            };
            // The patch's own average, corrected by the settings the picker returned.
            let mean = result["patch"]["mean_linear"]
                .as_array()
                .expect("the averaged patch");
            let mut row = [[
                mean[0].as_f64().unwrap() as f32,
                mean[1].as_f64().unwrap() as f32,
                mean[2].as_f64().unwrap() as f32,
            ]];
            for unit in operation.units() {
                unit.apply_row(0, 0, &mut row);
            }
            let codes = row[0].map(|value| {
                let clamped = f64::from(value).clamp(0.0, 1.0);
                let encoded = if clamped <= 0.003_130_8 {
                    12.92 * clamped
                } else {
                    1.055 * clamped.powf(1.0 / 2.4) - 0.055
                };
                (255.0 * encoded + 0.5).floor() as i32
            });
            let spread = codes.iter().max().unwrap() - codes.iter().min().unwrap();
            assert!(
                spread <= 1,
                "{}: corrected patch {codes:?} is not neutral to the code",
                case.description
            );
        }
    }

    /// A module that declares no queries says so rather than answering one.
    #[test]
    fn a_module_without_queries_refuses_the_call() {
        let module = crate::modules::TransformModule::new();
        assert!(module.descriptor().queries.is_empty());
        let error = module
            .query(
                NEUTRAL_SAMPLE,
                &Map::new(),
                &crate::modules::FixedStage::new(STAGE)
                    .context(&[], &crate::ModuleRegistry::builtin()),
            )
            .expect_err("no queries");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("declares no queries"), "{error}");
    }

    /// The module finds its own layer wherever the host placed it, including a stack that already
    /// holds a pixel replacement and a geometry tail.
    #[test]
    fn the_basic_layer_is_found_among_pixel_and_geometry_layers() {
        let pixel = Layer::pixel(1, 1, [1, 2, 3]);
        let orientation = Layer::orientation(Orientation::NEUTRAL);
        let basic = basic_layer(json!({"exposure": 1.0}));
        let stack = [pixel.clone(), basic.clone(), orientation.clone()];
        let locate = |layers: &[Layer]| {
            crate::ModuleRegistry::builtin()
                .own_layer(layers, BASIC_EFFECT, None)
                .unwrap()
                .map(|(_, layer)| layer.id.clone())
        };
        assert_eq!(locate(&stack), Some(basic.id.clone()));
        assert_eq!(locate(&[pixel, orientation]), None);
        assert!(PIXEL_EFFECT != BASIC_EFFECT && ORIENTATION_EFFECT != BASIC_EFFECT);
    }
}
