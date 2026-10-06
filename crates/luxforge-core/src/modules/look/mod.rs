//! The RAW look (`docs/design/raw-looks.md`): one colour-stage layer on a RAW photo holding the
//! look its picture starts from, a tone curve with a highlight shoulder, a chroma gain, a path to
//! white and an amount, all resolved into the payload so a render depends only on the recipe.
//!
//! This module implements [`ToolModule`] directly rather than as a field patch
//! ([`super::field_patch`]): its payload stores resolved data no action sets — knots whose `x` runs
//! past 1, the chroma gain, the knee and the fit's provenance — which a field patch, whose payload
//! keys are exactly its action's parameters and whose `curve` kind holds `[0, 1]` coordinates, cannot
//! carry. `set-look` is still a patch action (`patch: true`), so the host's generic check passes it
//! exactly the fields sent and the core draft lifecycle previews an Amount drag on the GPU with the
//! layer in its GPU shape.
//!
//! The layer is declared order 3 in the colour stage, after Basic (0) and before the Tone curve (5)
//! and the colour mixer (10), `single`, on RAW sources only and not maskable. It is not presettable:
//! neither action is a preset step, and [`ToolModule::settings`] captures nothing. The frozen Standard
//! look is [`standard`]; the one fused pointwise unit and its GPU program are [`unit`].
mod standard;
mod unit;

/// The look unit's GPU program, which [`super::GPU_PROGRAMS`] lists.
pub(crate) use unit::PROGRAM as LOOK_PROGRAM;

use super::{
    ActionDescriptor, ActionInput, ActionPlan, ColorOperation, CompileStage, Control,
    EffectDescriptor, EffectStage, LayerReport, LayerUpdate, ModuleDescriptor, NewLayer,
    ParameterDescriptor, PointwiseColor, Processing, ResetAction, StageContext, ToolModule,
    curve::{Interpolant, Tails},
};
use crate::{EFFECT_FORMAT, Error, Layer, SourceTag};
use serde_json::{Map, Value, json};
use standard::{STANDARD_CHROMA, STANDARD_KNEE, STANDARD_KNOTS};
use std::sync::Arc;
use unit::{Look, LookParameters};

/// The look's one pointwise unit, declared order 3 so a look layer follows the Basic layer and
/// precedes the Tone curve's.
pub const LOOK_EFFECT: &str = "luxforge.look.look";

/// The look module's identity.
const LOOK_MODULE: &str = "luxforge.look";

/// The patch that chooses the look and its amount.
const SET_LOOK: &str = "set-look";

/// The module reset: the Standard look at amount 100.
const RESET_LOOK: &str = "reset-look";

/// The sample query: the stored curve and the chroma gain.
const SAMPLE_QUERY: &str = "sample-look";

/// The sample query answers the curve at `x_max · i / SAMPLE_SEGMENTS` for `i` in
/// `0..=SAMPLE_SEGMENTS`.
const SAMPLE_SEGMENTS: usize = 256;

/// The payload's and `set-look`'s look choice, and its values.
const LOOK: &str = "look";
const STANDARD: &str = "standard";
const NEUTRAL: &str = "neutral";
/// Match camera, phase 2's look, which this build does not provide.
const CAMERA: &str = "camera";

/// The payload's and `set-look`'s amount: `0..=MAX_AMOUNT`, 100 the look itself.
const AMOUNT: &str = "amount";
const MAX_AMOUNT: f64 = 200.0;
const FULL_AMOUNT: f64 = 100.0;

/// The payload's resolved fields.
const TONE: &str = "tone";
const CHROMA: &str = "chroma";
const KNEE: &str = "knee";
const FIT: &str = "fit";

/// The most knots a look's tone curve holds.
const MAX_KNOTS: usize = 24;

/// Why `camera` is refused, wherever it is named.
const CAMERA_UNAVAILABLE: &str = "look camera (Match camera) is not available yet; it will be chosen through edit.match-camera-look";

/// A look as stored: the Neutral look, which is no look at all, or a resolved one.
#[derive(Clone, Debug, PartialEq)]
enum Payload {
    Neutral,
    Standard(Resolved),
}

/// A resolved look: its amount and the tone knots, chroma gain and knee written into the payload
/// when it was chosen.
#[derive(Clone, Debug, PartialEq)]
struct Resolved {
    amount: f64,
    tone: Vec<[f64; 2]>,
    chroma: f64,
    knee: f64,
}

impl Resolved {
    /// The current Standard look at `amount`.
    fn standard(amount: f64) -> Self {
        Self {
            amount,
            tone: STANDARD_KNOTS.to_vec(),
            chroma: STANDARD_CHROMA,
            knee: STANDARD_KNEE,
        }
    }

    /// Whether this look is the current Standard's knots, chroma and knee, whatever its amount.
    fn is_current_standard(&self) -> bool {
        self.tone == STANDARD_KNOTS && self.chroma == STANDARD_CHROMA && self.knee == STANDARD_KNEE
    }

    fn unit(&self, amount: f64) -> Look {
        Look::new(LookParameters {
            knots: &self.tone,
            chroma: self.chroma,
            knee: self.knee,
            amount,
        })
    }
}

impl Payload {
    /// The stored form, strictly: a Neutral payload is exactly `{"look": "neutral"}`; a Standard
    /// one holds exactly `look`, `amount` (`0..=200`), `tone` (2 to 24 finite knots, the first at
    /// `x = 0` with `y` in `[0, 1]`, `x` strictly increasing, `y` non-decreasing and the last at
    /// most 1), `chroma` (finite, above 0), `knee` (in `(0, 1)`) and `fit` (`null`). Anything else
    /// is refused by name and never rewritten.
    fn parse(value: &Value) -> Result<Self, Error> {
        let object = value
            .as_object()
            .ok_or_else(|| Error::validation("Look payload must be a JSON object"))?;
        let look = object
            .get(LOOK)
            .ok_or_else(|| Error::validation("Look payload lacks look"))?
            .as_str()
            .ok_or_else(|| Error::validation("Look payload's look must be a string"))?;
        let allowed: &[&str] = match look {
            NEUTRAL => &[LOOK],
            STANDARD => &[LOOK, AMOUNT, TONE, CHROMA, KNEE, FIT],
            CAMERA => return Err(Error::validation(CAMERA_UNAVAILABLE)),
            other => return Err(Error::validation(format!("unknown look {other}"))),
        };
        if let Some(name) = object.keys().find(|name| !allowed.contains(&name.as_str())) {
            return Err(Error::validation(format!(
                "unknown field {name} in a {look} Look payload"
            )));
        }
        if look == NEUTRAL {
            return Ok(Self::Neutral);
        }
        let field = |name: &str| {
            object
                .get(name)
                .ok_or_else(|| Error::validation(format!("Look payload lacks {name}")))
        };
        let number = |name: &str| {
            field(name)?
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| Error::validation(format!("Look {name} must be a finite number")))
        };
        let amount = number(AMOUNT)?;
        if !(0.0..=MAX_AMOUNT).contains(&amount) {
            return Err(Error::validation(format!(
                "Look amount {amount} is outside 0..={MAX_AMOUNT}"
            )));
        }
        let chroma = number(CHROMA)?;
        if chroma <= 0.0 {
            return Err(Error::validation(format!(
                "Look chroma {chroma} is not above 0"
            )));
        }
        let knee = number(KNEE)?;
        if !(knee > 0.0 && knee < 1.0) {
            return Err(Error::validation(format!(
                "Look knee {knee} is outside (0, 1)"
            )));
        }
        if !field(FIT)?.is_null() {
            return Err(Error::validation(
                "Look fit must be null on a standard look",
            ));
        }
        Ok(Self::Standard(Resolved {
            amount,
            tone: knots(field(TONE)?)?,
            chroma,
            knee,
        }))
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Neutral => json!({ LOOK: NEUTRAL }),
            Self::Standard(look) => json!({
                LOOK: STANDARD,
                AMOUNT: look.amount,
                TONE: look.tone,
                CHROMA: look.chroma,
                KNEE: look.knee,
                FIT: Value::Null,
            }),
        }
    }
}

/// A stored tone curve, checked as [`Payload::parse`] describes.
fn knots(value: &Value) -> Result<Vec<[f64; 2]>, Error> {
    let list = value
        .as_array()
        .ok_or_else(|| Error::validation("Look tone must be a list of knots"))?;
    if !(2..=MAX_KNOTS).contains(&list.len()) {
        return Err(Error::validation(format!(
            "Look tone holds 2 to {MAX_KNOTS} knots, not {}",
            list.len()
        )));
    }
    let knots = list
        .iter()
        .enumerate()
        .map(|(index, knot)| {
            knot.as_array()
                .filter(|pair| pair.len() == 2)
                .and_then(|pair| Some([pair[0].as_f64()?, pair[1].as_f64()?]))
                .filter(|pair| pair.iter().all(|value| value.is_finite()))
                .ok_or_else(|| {
                    Error::validation(format!("Look tone knot {index} is not two finite numbers"))
                })
        })
        .collect::<Result<Vec<[f64; 2]>, Error>>()?;
    let ([x0, y0], [_, last]) = (knots[0], knots[knots.len() - 1]);
    if x0 != 0.0 {
        return Err(Error::validation("Look tone's first knot is not at x = 0"));
    }
    if !(0.0..=1.0).contains(&y0) {
        return Err(Error::validation(
            "Look tone's first knot's y is outside [0, 1]",
        ));
    }
    if last > 1.0 {
        return Err(Error::validation("Look tone's last knot's y is above 1"));
    }
    for (index, pair) in knots.windows(2).enumerate() {
        if pair[1][0] <= pair[0][0] {
            return Err(Error::validation(format!(
                "Look tone knot {}'s x does not increase",
                index + 1
            )));
        }
        if pair[1][1] < pair[0][1] {
            return Err(Error::validation(format!(
                "Look tone knot {}'s y decreases",
                index + 1
            )));
        }
    }
    Ok(knots)
}

/// The stored look of a layer of this module's effect at the current format.
fn read(effect_id: &str, format: u32, payload: &Value) -> Result<Payload, Error> {
    if effect_id != LOOK_EFFECT {
        return Err(Error::unavailable_effect(effect_id, &[]));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::incompatible(format!(
            "unsupported Look effect format {format}"
        )));
    }
    Payload::parse(payload)
}

fn stored(layer: &Layer) -> Result<Payload, Error> {
    read(&layer.effect_id, layer.effect_format, &layer.payload)
}

/// `Look amount 80`: an amount as history and the recipe row show it, with no decimals.
fn amount_words(amount: f64) -> String {
    format!("Look amount {amount:.0}")
}

/// The RAW look module.
#[derive(Debug)]
pub(crate) struct LookModule {
    descriptor: ModuleDescriptor,
}

impl LookModule {
    pub(crate) fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: LOOK_MODULE.into(),
                title: "Look".into(),
                hint: Some("The RAW photo's starting rendition".into()),
                effects: vec![EffectDescriptor {
                    order: 3,
                    single: true,
                    sources: vec![SourceTag::Raw],
                    ..EffectDescriptor::new(LOOK_EFFECT, EffectStage::Color)
                }],
                actions: vec![
                    ActionDescriptor {
                        patch: true,
                        preset: false,
                        parameters: vec![
                            ParameterDescriptor::enumeration(LOOK, [STANDARD, NEUTRAL]).notes(
                                "standard writes the current Standard look's resolved knots, \
                                 chroma gain and knee into the layer, keeping the amount in force \
                                 (100 from Neutral); neutral writes the Neutral look, the bare RAW \
                                 development. Match camera is not available yet",
                            ),
                            ParameterDescriptor::number(AMOUNT, 0.0, MAX_AMOUNT)
                                .default(FULL_AMOUNT)
                                .step(1.0)
                                .precision(0)
                                .notes(
                                    "how much of the look applies, blended with the look's input \
                                     in linear light: 0 none, 100 the look, past 100 extrapolated. \
                                     A Neutral look has no amount, so an amount beside look \
                                     neutral, or alone on a Neutral look, is refused",
                                ),
                        ],
                        ..ActionDescriptor::new(
                            SET_LOOK,
                            "Set look",
                            "sets the RAW photo's look in its one Look layer, after Basic and \
                             before the Tone curve, adding the layer on the first set that is not \
                             Neutral. A field not sent keeps the look in force; a set that \
                             changes nothing is a reported no-op",
                        )
                    },
                    ActionDescriptor {
                        preset: false,
                        ..ActionDescriptor::new(
                            RESET_LOOK,
                            "Reset Look",
                            "returns the look to the current Standard look at amount 100, keeping \
                             the Look layer's identity, or adds it to a photo without one",
                        )
                    },
                ],
                queries: vec![ActionDescriptor {
                    preset: false,
                    ..ActionDescriptor::new(
                        SAMPLE_QUERY,
                        "Sample look",
                        "the stored look's tone curve, 257 [x, T(x)] samples of encoded luminance \
                         over [0, x_max], its chroma gain and amount, read from the payload \
                         without a pixel; a photo whose look is Neutral answers the identity over \
                         [0, 1] and a gain of 1",
                    )
                }],
                controls: vec![
                    Control::choice(SET_LOOK, LOOK, "Look").into(),
                    Control::number(SET_LOOK, AMOUNT, "Amount").into(),
                ],
                reset: Some(ResetAction {
                    action: RESET_LOOK.into(),
                    preset: Map::new(),
                }),
                ..ModuleDescriptor::default()
            },
        }
    }
}

impl ToolModule for LookModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// `set-look` stores exactly the fields sent, after refusing what names no look: an amount
    /// beside `look: neutral`, and Match camera, which this build does not provide.
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        match action_id {
            SET_LOOK => match parameters.get(LOOK).and_then(Value::as_str) {
                Some(NEUTRAL) if parameters.contains_key(AMOUNT) => {
                    return Err(Error::validation(
                        "a Neutral look has no amount; send look standard with an amount",
                    ));
                }
                Some(CAMERA) => return Err(Error::validation(CAMERA_UNAVAILABLE)),
                _ => {}
            },
            RESET_LOOK => {}
            other => return Err(Error::validation(format!("unknown Look action {other}"))),
        }
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }

    /// The look a request leaves, against the stored one (none is the Neutral look): `reset-look`
    /// the current Standard at 100; `look: standard` the current Standard's knots at the amount
    /// sent, or the one in force, or 100 from Neutral; `look: neutral` the Neutral look; an amount
    /// alone the stored look at that amount, refused on a Neutral one. A Neutral look on a photo
    /// without a layer, or the look already stored, is a no-op. Reads the payload only.
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        if context.kind != SourceTag::Raw {
            return Err(Error::validation("the Look applies to RAW photos only"));
        }
        let existing = context.own_layer(LOOK_EFFECT)?.map(|(_, layer)| layer);
        let current = existing.map(stored).transpose()?;
        let parameters = &input.parameters;
        let amount = parameters.get(AMOUNT).and_then(Value::as_f64);
        let next = match input.action_id.as_str() {
            RESET_LOOK => Payload::Standard(Resolved::standard(FULL_AMOUNT)),
            SET_LOOK => match parameters.get(LOOK).and_then(Value::as_str) {
                Some(STANDARD) => {
                    let in_force = match &current {
                        Some(Payload::Standard(look)) => look.amount,
                        _ => FULL_AMOUNT,
                    };
                    Payload::Standard(Resolved::standard(amount.unwrap_or(in_force)))
                }
                Some(NEUTRAL) => Payload::Neutral,
                Some(CAMERA) => return Err(Error::validation(CAMERA_UNAVAILABLE)),
                Some(other) => return Err(Error::validation(format!("unknown look {other}"))),
                None => match (amount, &current) {
                    (None, _) => return Ok(ActionPlan::NoOp),
                    (Some(amount), Some(Payload::Standard(look))) => Payload::Standard(Resolved {
                        amount,
                        ..look.clone()
                    }),
                    (Some(_), _) => {
                        return Err(Error::validation(
                            "a Neutral look has no amount; choose look standard first",
                        ));
                    }
                },
            },
            other => return Err(Error::validation(format!("unknown Look action {other}"))),
        };
        Ok(match (existing, current) {
            (None, _) if next == Payload::Neutral => ActionPlan::NoOp,
            (None, _) => ActionPlan::Commit(NewLayer::new(LOOK_EFFECT, next.to_value())),
            (Some(_), Some(current)) if current == next => ActionPlan::NoOp,
            (Some(layer), _) => {
                ActionPlan::Update(LayerUpdate::new(layer.id.clone(), next.to_value()))
            }
        })
    }

    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        read(effect_id, format, payload).map(drop)
    }

    /// `Look Standard` (with its amount when it is not 100) or `Look Neutral`, the values `look`,
    /// `amount`, `chroma`, the knot count `knots` and `fit`, and neutral exactly when the look is
    /// the current Standard at amount 100 or Neutral: the look a new RAW photo starts from or none,
    /// so a client's edited mark stays dark for it. A neutral report still renders the look.
    fn describe(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<LayerReport, Error> {
        Ok(match read(effect_id, format, payload)? {
            Payload::Neutral => LayerReport {
                summary: "Look Neutral".into(),
                values: Map::from_iter([(LOOK.into(), json!(NEUTRAL))]),
                neutral: true,
            },
            Payload::Standard(look) => LayerReport {
                summary: if look.amount == FULL_AMOUNT {
                    "Look Standard".into()
                } else {
                    format!("Look Standard, amount {:.0}", look.amount)
                },
                values: Map::from_iter([
                    (LOOK.into(), json!(STANDARD)),
                    (AMOUNT.into(), json!(look.amount)),
                    (CHROMA.into(), json!(look.chroma)),
                    ("knots".into(), json!(look.tone.len())),
                    (FIT.into(), Value::Null),
                ]),
                neutral: look.is_current_standard() && look.amount == FULL_AMOUNT,
            },
        })
    }

    /// `Look Standard`, `Look Neutral`, `Look amount 80` and `Reset Look`.
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        let parameters = &input.parameters;
        match input.action_id.as_str() {
            RESET_LOOK => "Reset Look".into(),
            SET_LOOK => match (
                parameters.get(LOOK).and_then(Value::as_str),
                parameters.get(AMOUNT).and_then(Value::as_f64),
            ) {
                (Some(STANDARD), _) => "Look Standard".into(),
                (Some(NEUTRAL), _) => "Look Neutral".into(),
                (None, Some(amount)) => amount_words(amount),
                _ => action.title.clone(),
            },
            _ => action.title.clone(),
        }
    }

    /// The look is not presettable: a preset captures nothing of it.
    fn settings(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        read(effect_id, format, payload).map(|_| Map::new())
    }

    /// 257 samples of the stored tone curve over `[0, x_max]`, computed in `f64` by the
    /// interpolant the unit casts its coefficients from, with the chroma gain and the amount. The
    /// answer depends only on the stored payload: no pixel is read and no frame is allocated.
    fn query(
        &self,
        query_id: &str,
        _: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if query_id != SAMPLE_QUERY {
            return Err(Error::validation(format!("unknown query {query_id}")));
        }
        let payload = match context.own_layer(LOOK_EFFECT)? {
            Some((_, layer)) => stored(layer)?,
            None => Payload::Neutral,
        };
        let sampled = |x_max: f64, value: &dyn Fn(f64) -> f64| -> Vec<Value> {
            (0..=SAMPLE_SEGMENTS)
                .map(|index| {
                    let x = x_max * index as f64 / SAMPLE_SEGMENTS as f64;
                    json!([x, value(x)])
                })
                .collect()
        };
        Ok(match payload {
            Payload::Neutral => json!({
                LOOK: NEUTRAL,
                "points": sampled(1.0, &|x| x),
                CHROMA: 1.0,
            }),
            Payload::Standard(look) => {
                let curve = Interpolant::with_tails(&look.tone, Tails::Look);
                let x_max = look.tone[look.tone.len() - 1][0];
                json!({
                    LOOK: STANDARD,
                    "points": sampled(x_max, &|x| curve.value(x)),
                    CHROMA: look.chroma,
                    AMOUNT: look.amount,
                })
            }
        })
    }

    /// The Neutral look, which a GPU preview plans in place of a look layer the stack does not
    /// hold yet: in the GPU shape it is the look's one unit as the identity.
    fn neutral_payload(&self, _: &str) -> Value {
        Payload::Neutral.to_value()
    }

    /// The Neutral look and amount 0 compile to no units, the identity path. Every other look is
    /// one unit. In the GPU shape (`CompileStage::gpu_shape`) every look is that one unit, a
    /// Neutral look or amount 0 the unit at amount 0, which is the identity, so an Amount drag to
    /// or from 0 keeps one program sequence; no CPU compile asks for that shape.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: CompileStage,
    ) -> Result<Processing, Error> {
        let unit = match read(effect_id, format, payload)? {
            Payload::Standard(look) if look.amount != 0.0 => Some(look.unit(look.amount)),
            Payload::Standard(look) if at.gpu_shape => Some(look.unit(0.0)),
            Payload::Neutral if at.gpu_shape => Some(Resolved::standard(0.0).unit(0.0)),
            _ => None,
        };
        Ok(Processing::Color(match unit {
            Some(unit) => {
                let unit: Arc<dyn PointwiseColor> = Arc::new(unit);
                ColorOperation::new(vec![unit])
            }
            None => ColorOperation::neutral(),
        }))
    }
}

#[cfg(test)]
mod tests;
