//! Developer proof for the complete first-slice control vocabulary, as a field-patch module: a
//! field of every kind a field patch holds, drawn by a control of every kind and style, with the
//! action buttons and the two-channel curve its group lists beside them, and the curve's sample
//! query. It shares every field-patch rule with Basic and the other modules; its stored layer
//! describes control values but compiles to an identity colour operation, so it cannot change
//! photo pixels.
use super::{
    ActionDescriptor, ActionStyle, ChoiceStyle, ColorOperation, ColorStyle, Control,
    CurveBackground, CurveChannel, EffectStage, NumberStyle, ParameterDescriptor, Processing,
    RailDecoration, StageContext,
    field_patch::{Field, FieldControl, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::Error;
use serde_json::{Map, Value, json};

pub const CONTROLS_EFFECT: &str = "luxforge.controls.identity";
pub(super) const SET_CONTROLS: &str = "set-controls";
pub(super) const SAMPLE_CONTROLS_CURVE: &str = "sample-controls-curve";
const SAMPLE_SEGMENTS: usize = 256;
const NOTES: &str = "Developer control parity fixture; values never alter pixels";
/// The two curve fields, which are the curve control's channels and the sample query's parameters.
const CURVES: [(&str, &str, bool); 2] = [("master", "Master", true), ("red", "Red", false)];

/// The controls proof's table, identity compilation and curve sampling.
#[derive(Debug, Default)]
pub struct Controls;

/// The developer controls proof: `Controls` as a field-patch module.
pub type ControlsModule = FieldPatchModule<Controls>;

/// A curve parameter of 2 to 8 points in 0.01 steps, monotone when `monotone`.
fn curve(name: &str, monotone: bool) -> ParameterDescriptor {
    let curve = ParameterDescriptor::curve(name, 2, 8)
        .step(0.01)
        .notes(NOTES);
    if monotone { curve.monotone() } else { curve }
}

/// A field of `parameter` with the proof's notes.
fn field(parameter: ParameterDescriptor, label: &str) -> Field {
    Field::new(parameter.notes(NOTES), label)
}

fn preset(name: &str, value: Value) -> Map<String, Value> {
    Map::from_iter([(name.to_owned(), value)])
}

impl FieldPatch for Controls {
    fn spec() -> Spec {
        let modes = ["one", "two", "three"];
        let more = ["one", "two", "three", "four", "five"];
        let identity = json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
        let mut fields = vec![
            field(
                ParameterDescriptor::number("amount", -10.0, 10.0)
                    .soft_min(-5.0)
                    .soft_max(5.0)
                    .step(0.1)
                    .fine_step(0.01)
                    .zero(0.0)
                    .precision(2)
                    .default(0.0),
                "Amount",
            )
            .rail(RailDecoration::Temperature),
            field(
                ParameterDescriptor::number("coordinate", 0.0, 100.0)
                    .step(1.0)
                    .precision(0)
                    .default(50.0),
                "Coordinate",
            )
            .control(FieldControl::Number(NumberStyle::Field)),
            field(
                ParameterDescriptor::integer("count", 0, 20)
                    .step(1.0)
                    .default(0),
                "Count",
            )
            .control(FieldControl::Number(NumberStyle::Stepper)),
            field(
                ParameterDescriptor::boolean("enabled").default(false),
                "Enabled",
            ),
            field(
                ParameterDescriptor::enumeration("mode", modes).default("one"),
                "Mode",
            )
            .control(FieldControl::Choice(ChoiceStyle::Segmented)),
            field(
                ParameterDescriptor::enumeration("mode-chips", more).default("one"),
                "Mode chips",
            )
            .control(FieldControl::Choice(ChoiceStyle::Chips)),
            field(
                ParameterDescriptor::enumeration("mode-menu", more).default("one"),
                "Mode menu",
            )
            .control(FieldControl::Choice(ChoiceStyle::Menu)),
            field(
                ParameterDescriptor::color("rgb").default(json!([64, 128, 192])),
                "Colour",
            )
            .control(FieldControl::Color(ColorStyle::Picker)),
            field(
                ParameterDescriptor::color("rgb-fields").default(json!([0, 0, 0])),
                "RGB fields",
            ),
        ];
        fields.extend(CURVES.map(|(name, label, monotone)| {
            field(curve(name, monotone).default(identity.clone()), label)
        }));
        let names: Vec<&'static str> = [
            "amount",
            "coordinate",
            "count",
            "enabled",
            "mode",
            "mode-chips",
            "mode-menu",
            "rgb",
            "rgb-fields",
        ]
        .into_iter()
        .chain(CURVES.map(|(name, _, _)| name))
        .collect();
        Spec::new(
            "luxforge.controls",
            "Controls",
            "Developer control vocabulary",
            CONTROLS_EFFECT,
            EffectStage::Color,
        )
        .set_notes("One field patch for every generated control")
        .reset_notes("Clear all proof control values")
        .fields(fields)
        // The curve control draws the two curve fields as its channels; the three action buttons,
        // one per style, each send a one-field patch.
        .group(
            Group::new("Control vocabulary", names)
                .extra(
                    Control::curve(
                        SET_CONTROLS,
                        CURVES
                            .map(|(name, label, _)| CurveChannel {
                                parameter: name.into(),
                                label: label.into(),
                            })
                            .into(),
                        "Curve",
                        SAMPLE_CONTROLS_CURVE,
                    )
                    .background(CurveBackground::Histogram),
                )
                .extra(
                    Control::action(SET_CONTROLS, "Enable").preset(preset("enabled", json!(true))),
                )
                .extra(
                    Control::action(SET_CONTROLS, "Mode two")
                        .preset(preset("mode", json!("two")))
                        .action_style(ActionStyle::Primary),
                )
                .extra(
                    Control::action(SET_CONTROLS, "Reset amount")
                        .preset(preset("amount", json!(0.0)))
                        .action_style(ActionStyle::Icon)
                        .icon("reset"),
                ),
        )
        .query(ActionDescriptor {
            parameters: CURVES
                .map(|(name, _, monotone)| curve(name, monotone))
                .into(),
            ..ActionDescriptor::new(
                SAMPLE_CONTROLS_CURVE,
                "Sample controls curve",
                "257 linearly interpolated fractions from the one submitted channel",
            )
        })
        .developer()
    }

    fn compile(&self, _: &Values<'_>, _: crate::CompileStage) -> Result<Processing, Error> {
        Ok(Processing::Color(ColorOperation::neutral()))
    }

    /// 257 samples of the one channel sent, linearly interpolated between its points and held flat
    /// outside them. The host has checked the channel against its curve declaration.
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        _: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if query_id != SAMPLE_CONTROLS_CURVE {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        let Some(points) = parameters
            .values()
            .next()
            .filter(|_| parameters.len() == 1)
            .and_then(Value::as_array)
        else {
            return Err(Error::validation(
                "sample-controls-curve requires exactly one channel parameter",
            ));
        };
        let vertices: Vec<[f64; 2]> = points
            .iter()
            .filter_map(|point| {
                let pair = point.as_array()?;
                Some([pair.first()?.as_f64()?, pair.get(1)?.as_f64()?])
            })
            .collect();
        let (Some(first), Some(last)) = (vertices.first(), vertices.last()) else {
            return Err(Error::validation(
                "sample-controls-curve requires a checked curve channel",
            ));
        };
        let sampled: Vec<Value> = (0..=SAMPLE_SEGMENTS)
            .map(|index| {
                let x = index as f64 / SAMPLE_SEGMENTS as f64;
                let y = if x <= first[0] {
                    first[1]
                } else if x >= last[0] {
                    last[1]
                } else {
                    let pair = vertices
                        .windows(2)
                        .find(|pair| x >= pair[0][0] && x <= pair[1][0])
                        .expect("ordered curve spans x");
                    let t = (x - pair[0][0]) / (pair[1][0] - pair[0][0]);
                    pair[0][1] + t * (pair[1][1] - pair[0][1])
                };
                json!([x, y])
            })
            .collect();
        Ok(json!({"points": sampled}))
    }
}
