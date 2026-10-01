//! The Tone curve module: one colour-stage layer holding a monotone point curve over the
//! photograph's encoded luminance, edited by one field-patch action (`docs/design/tone-curve.md`).
//!
//! This is a field-patch module ([`super::field_patch`]): the payload is a JSON object whose one
//! key, `luminance`, holds the points the host's `curve` kind has checked, a missing key is the
//! identity `[[0, 0], [1, 1]]`, and the canonical neutral payload is `{}`. The module owns exactly
//! one layer of `luxforge.curve.tone`, declared order 5 so the layer always lands after the Basic
//! layer and before the colour mixer's in the colour run. The interpolant and the one pointwise unit
//! are [`unit::Interpolant`] and [`unit::ToneCurve`]; this file owns the field table, the
//! compilation into that unit and the curve control's sample query.
mod unit;

use super::{
    ActionDescriptor, ColorOperation, CompileStage, Control, CurveBackground, CurveChannel,
    EffectStage, ParameterDescriptor, PointwiseColor, Processing, StageContext,
    field_patch::{Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::Error;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use unit::{Interpolant, ToneCurve};

/// The Tone curve's one pointwise unit: a monotone point curve over encoded luminance, declared
/// order 5 so a curve layer follows the Basic layer and precedes the colour mixer's.
pub const CURVE_EFFECT: &str = "luxforge.curve.tone";

/// The most points the curve holds.
pub(crate) const MAX_POINTS: usize = 16;

/// The curve control's sample query.
pub(crate) const SAMPLE_QUERY: &str = "sample-curve";

/// The sample query answers the curve at `i / SAMPLE_SEGMENTS` for `i` in `0..=SAMPLE_SEGMENTS`.
pub(crate) const SAMPLE_SEGMENTS: usize = 256;

/// The field patch the curve control sends.
const SET_CURVE: &str = "set-curve";

/// The one field, the curve control's one channel and the sample query's one parameter.
const LUMINANCE: &str = "luminance";

/// The Tone curve's table, compilation and sample query.
#[derive(Debug, Default)]
pub(crate) struct Curve;

/// The Tone curve module: `Curve` as a field-patch module.
pub(crate) type CurveModule = FieldPatchModule<Curve>;

impl FieldPatch for Curve {
    fn spec() -> Spec {
        Spec::new(
            "luxforge.curve",
            "Tone curve",
            "A point curve over the photograph's tones",
            CURVE_EFFECT,
            EffectStage::Color,
        )
        .order(5)
        .maskable()
        .set_notes("A monotone point curve over encoded luminance, placed after Basic and before the colour mixer by declared order")
        .fields([Field::new(
            ParameterDescriptor::curve(LUMINANCE, 2, MAX_POINTS)
                .monotone()
                .step(0.01)
                .precision(2)
                .default(json!([[0, 0], [1, 1]])),
            "Luminance",
        )
        .history("Tone curve")])
        .group(
            Group::new("Tone curve", [LUMINANCE]).extra(
                Control::curve(
                    SET_CURVE,
                    vec![CurveChannel {
                        parameter: LUMINANCE.into(),
                        label: "Luminance".into(),
                    }],
                    "Tone curve",
                    SAMPLE_QUERY,
                )
                .background(CurveBackground::Histogram),
            ),
        )
        .query(ActionDescriptor {
            parameters: vec![ParameterDescriptor::curve(LUMINANCE, 2, MAX_POINTS).monotone()],
            ..ActionDescriptor::new(
                SAMPLE_QUERY,
                "Sample tone curve",
                "257 samples of the curve through the submitted points",
            )
        })
        .collapsed()
    }

    /// Only a layer whose points differ from the default reaches here. Points exactly on the
    /// diagonal are still the identity map, which compiles to no units so the layer keeps the
    /// identity byte path; every other curve is one unit.
    fn compile(&self, values: &Values<'_>, _: CompileStage) -> Result<Processing, Error> {
        let points = values.curve(LUMINANCE);
        if Interpolant::is_identity(&points) {
            return Ok(Processing::Color(ColorOperation::neutral()));
        }
        let curve: Arc<dyn PointwiseColor> = Arc::new(ToneCurve::new(&Interpolant::new(&points)));
        Ok(Processing::Color(ColorOperation::new(vec![curve])))
    }

    /// 257 samples of the curve through the submitted points, computed in `f64` by the
    /// interpolant production casts its coefficients from. The answer depends only on the points:
    /// `entry_id` and `mask` select nothing, no pixel is read and no frame is allocated.
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        _: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if query_id != SAMPLE_QUERY {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        let points = parameters
            .get(LUMINANCE)
            .and_then(Value::as_array)
            .map(|points| {
                points
                    .iter()
                    .map(|point| {
                        let pair = point.as_array()?;
                        Some([pair.first()?.as_f64()?, pair.get(1)?.as_f64()?])
                    })
                    .collect::<Option<Vec<[f64; 2]>>>()
            });
        let Some(Some(points)) = points else {
            return Err(Error::validation(
                "sample-curve requires the luminance points",
            ));
        };
        let curve = Interpolant::new(&points);
        let sampled: Vec<Value> = (0..=SAMPLE_SEGMENTS)
            .map(|index| {
                let x = index as f64 / SAMPLE_SEGMENTS as f64;
                json!([x, curve.value(x)])
            })
            .collect();
        Ok(json!({"points": sampled}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModuleRegistry;
    use crate::modules::{ActionInput, FixedStage, Stage, ToolModule};
    use luxforge_reference::curve::{CurvePoints, curve as reference_curve};

    fn module() -> CurveModule {
        CurveModule::new()
    }

    fn label(action: &str, parameters: Value) -> String {
        let module = module();
        module.label(
            module.descriptor().action(action).unwrap(),
            &ActionInput {
                action_id: action.to_owned(),
                parameters: parameters.as_object().cloned().unwrap(),
            },
        )
    }

    fn sample(parameters: Value) -> Result<Value, Error> {
        let registry = ModuleRegistry::new();
        let stage = FixedStage::new(Stage {
            width: 4,
            height: 4,
        });
        module().query(
            SAMPLE_QUERY,
            parameters.as_object().unwrap(),
            &stage.context(&[], &registry),
        )
    }

    /// A field patch is labelled by its point count; one that returns the curve to its default,
    /// however spelled, and the module reset are `Reset Tone curve`; the recipe row reads the same
    /// words, or `Neutral`.
    #[test]
    fn labels_read_tone_curve_n_points_and_reset_tone_curve() {
        assert_eq!(
            label(
                SET_CURVE,
                json!({"luminance": [[0, 0], [0.5, 0.6], [1, 1]]})
            ),
            "Tone curve 3 points"
        );
        assert_eq!(
            label(SET_CURVE, json!({"luminance": [[0, 0.1], [1, 1]]})),
            "Tone curve 2 points"
        );
        assert_eq!(
            label(SET_CURVE, json!({"luminance": [[0, 0], [1.0, 1]]})),
            "Reset Tone curve"
        );
        assert_eq!(label("reset-curve", json!({})), "Reset Tone curve");
        let module = module();
        let describe = |payload: Value| module.describe(CURVE_EFFECT, 1, &payload).unwrap().summary;
        assert_eq!(
            describe(json!({"luminance": [[0, 0], [0.5, 0.6], [1, 1]]})),
            "Tone curve 3 points"
        );
        assert_eq!(describe(json!({})), "Neutral");
    }

    /// The query answers 257 `[i / 256, C(i / 256)]` pairs of the submitted points, whatever the
    /// entry or mask, equal to the independent reference.
    #[test]
    fn sample_curve_returns_257_samples_of_the_submitted_points() {
        let points = [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]];
        let reference = CurvePoints::new(&points);
        let answer = sample(json!({"luminance": points})).unwrap();
        let sampled = answer["points"].as_array().unwrap();
        assert_eq!(sampled.len(), SAMPLE_SEGMENTS + 1);
        for (index, pair) in sampled.iter().enumerate() {
            let x = pair[0].as_f64().unwrap();
            let y = pair[1].as_f64().unwrap();
            assert_eq!(x, index as f64 / 256.0);
            let expected = reference_curve(&reference, x);
            assert!((y - expected).abs() <= 1e-12, "{x}: {y} against {expected}");
        }
        assert_eq!(sampled[0], json!([0.0, 0.0]));
        assert_eq!(sampled[64], json!([0.25, 0.2]));
        assert_eq!(sampled[256], json!([1.0, 1.0]));

        let identity = sample(json!({"luminance": [[0, 0], [0.5, 0.5], [1, 1]]})).unwrap();
        for pair in identity["points"].as_array().unwrap() {
            assert_eq!(pair[0], pair[1]);
        }
    }

    #[test]
    fn sample_curve_without_points_is_refused() {
        let error = sample(json!({})).unwrap_err();
        assert_eq!(error.detail, "sample-curve requires the luminance points");
        let error = module()
            .query(
                "sample-other",
                &Map::new(),
                &FixedStage::new(Stage {
                    width: 4,
                    height: 4,
                })
                .context(&[], &ModuleRegistry::new()),
            )
            .unwrap_err();
        assert_eq!(error.detail, "unknown query sample-other");
    }
}
