//! The colour mixer module: one colour-stage layer holding hue, saturation and luminance for each
//! of eight colour ranges, edited by one field-patch action.
//!
//! This is a field-patch module (`docs/design/modules-and-api.md`'s field-patch contract, shared in
//! [`super::field_patch`]): a payload is a JSON object whose keys are the implemented parameter
//! names, a missing key is neutral, and the canonical neutral payload is the empty object `{}`. The
//! module owns exactly one layer of `luxforge.mixer.hsl`, declared order 10 so a mixer layer
//! always follows the Basic layer in the colour run (`docs/design/presence-mixer-vignette.md`,
//! "Placement and stage order"). The one pointwise unit's equations are frozen in
//! `docs/design/mixer-study.md` and implemented in [`unit::Mixer`]; this file owns only the field
//! table, the controls' rails and the compilation into that unit.
mod unit;

use super::{
    ColorOperation, EffectStage, PointwiseColor, Processing, RailDecoration, Stage,
    field_patch::{Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::Error;
use std::sync::Arc;

/// The colour mixer's one pointwise unit: hue, saturation and luminance for the eight colour
/// ranges, declared order 10 so a mixer layer always follows the Basic layer in the colour run.
pub const MIXER_EFFECT: &str = "luxforge.mixer.hsl";

const HUE: &str = "hue";
const SATURATION: &str = "saturation";
const LUMINANCE: &str = "luminance";

/// Every implemented mixer field: the hue group, then saturation, then luminance, each in range
/// order red, orange, yellow, green, aqua, blue, purple, magenta. This is the payload's declared
/// order, the controls' rendering order and the order [`unit::Mixer::new`] expects its three
/// eight-element arrays in.
///
/// Names are `<range>-<property>`, hyphen-separated: [`crate::modules::valid_name`] is the shared
/// identity rule every action and parameter name is checked against (lowercase words joined by
/// `-`), so `red-hue` is what actually registers.
const FIELDS: [&str; 24] = [
    "red-hue",
    "orange-hue",
    "yellow-hue",
    "green-hue",
    "aqua-hue",
    "blue-hue",
    "purple-hue",
    "magenta-hue",
    "red-saturation",
    "orange-saturation",
    "yellow-saturation",
    "green-saturation",
    "aqua-saturation",
    "blue-saturation",
    "purple-saturation",
    "magenta-saturation",
    "red-luminance",
    "orange-luminance",
    "yellow-luminance",
    "green-luminance",
    "aqua-luminance",
    "blue-luminance",
    "purple-luminance",
    "magenta-luminance",
];

/// The group label a patch that returns every one of a property's eight fields to neutral takes in
/// history, and the label the group's own control carries.
const HUE_GROUP: &str = "Hue";
const SATURATION_GROUP: &str = "Saturation";
const LUMINANCE_GROUP: &str = "Luminance";

/// The neutral value of every mixer field.
const NEUTRAL: f64 = 0.0;

/// A declared field's range index and property, parsed from its `<range>-<property>` name.
fn parse_field(name: &str) -> Option<(usize, &'static str)> {
    let (range_name, property) = name.split_once('-')?;
    let range = unit::RANGE_NAMES
        .iter()
        .position(|candidate| *candidate == range_name)?;
    let property = match property {
        "hue" => HUE,
        "saturation" => SATURATION,
        "luminance" => LUMINANCE,
        _ => return None,
    };
    Some((range, property))
}

/// The capitalized range name a control label and a history label both use, e.g. `Red`.
fn range_label(range: usize) -> String {
    let name = unit::RANGE_NAMES[range];
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The gradient rail for a colour's reference sRGB code, at one quarter (dark) and 60% of the way
/// to white (light).
fn dark(colour: [u8; 3]) -> [u8; 3] {
    colour.map(|channel| channel / 4)
}

fn light(colour: [u8; 3]) -> [u8; 3] {
    colour.map(|channel| {
        let channel = f32::from(channel);
        (channel + (255.0 - channel) * 0.6)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

const MID_GREY: [u8; 3] = [128, 128, 128];

/// The previous, this and next range's reference colours: `+100` rotates toward the next centre
/// and `-100` toward the previous one, so the rail shows both destinations either side of home.
fn hue_rail(range: usize) -> RailDecoration {
    let previous = unit::RANGE_REFERENCE_CODES[(range + unit::RANGE_COUNT - 1) % unit::RANGE_COUNT];
    let this = unit::RANGE_REFERENCE_CODES[range];
    let next = unit::RANGE_REFERENCE_CODES[(range + 1) % unit::RANGE_COUNT];
    RailDecoration::Gradient {
        stops: vec![previous, this, next],
    }
}

/// Mid grey at `-100` (zero chroma) to the range's own colour at `+100` (double chroma).
fn saturation_rail(range: usize) -> RailDecoration {
    RailDecoration::Gradient {
        stops: vec![MID_GREY, unit::RANGE_REFERENCE_CODES[range]],
    }
}

/// A dark version of the range's colour at `-100`, the colour itself at `0`, a light version at
/// `+100`.
fn luminance_rail(range: usize) -> RailDecoration {
    let colour = unit::RANGE_REFERENCE_CODES[range];
    RailDecoration::Gradient {
        stops: vec![dark(colour), colour, light(colour)],
    }
}

/// One `<range>-<property>` field: -100..100, step 1, no display decimals, no unit, a zero hint,
/// the range's name on its slider and the range's colours on its rail. Its history label names
/// the range and the property, e.g. `Red hue +20`.
fn mixer_field(name: &'static str) -> Field {
    let (range, property) = parse_field(name).expect("a declared mixer field");
    let label = range_label(range);
    let (notes, rail) = match property {
        HUE => (
            format!(
                "moves the {label} range's hues toward a neighbouring range: +100 carries its centre colour 85% of the way to the next range's centre and -100 85% of the way to the previous one; two neighbours driven at each other share one slider's travel"
            ),
            hue_rail(range),
        ),
        SATURATION => (
            format!(
                "scales chroma within the {label} range; -100 is exactly neutral grey for that range's own colour and +100 doubles chroma; every saturation slider at -100 makes the whole photo exactly grey"
            ),
            saturation_rail(range),
        ),
        LUMINANCE => (
            format!(
                "scales Oklab L within the {label} range through a compressive response with the near-black rule"
            ),
            luminance_rail(range),
        ),
        _ => unreachable!("parse_field returns only hue, saturation or luminance"),
    };
    Field::slider(name, label.as_str(), notes)
        .history(format!("{label} {property}"))
        .zero(NEUTRAL)
        .rail(rail)
}

/// The colour mixer's table and compilation.
#[derive(Debug, Default)]
pub(crate) struct Mixer;

/// The colour mixer module: `Mixer` as a field-patch module.
pub(crate) type MixerModule = FieldPatchModule<Mixer>;

impl FieldPatch for Mixer {
    fn spec() -> Spec {
        Spec::new(
            "luxforge.mixer",
            "Colour mixer",
            "Hue, saturation and luminance by range",
            MIXER_EFFECT,
            EffectStage::Color,
        )
        .order(10)
        .maskable()
        .set_notes("merges the named mixer fields into the stack's one Colour mixer layer, which the host places after Basic by declared order on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op")
        .fields(FIELDS.map(mixer_field))
        .group(Group::new(HUE_GROUP, FIELDS[0..8].iter().copied()))
        .group(Group::new(SATURATION_GROUP, FIELDS[8..16].iter().copied()).collapsed())
        .group(Group::new(LUMINANCE_GROUP, FIELDS[16..24].iter().copied()).collapsed())
        .collapsed()
        // The three groups are parallel views of the same eight ranges, so the desktop draws them
        // as one segmented row instead of stacked sections.
        .layout(crate::ModuleLayout::Tabs)
    }

    /// Only a layer with a moved field reaches here; the shared field patch compiles a neutral one
    /// to no units.
    fn compile(&self, values: &Values<'_>, _: Stage) -> Result<Processing, Error> {
        // FIELDS is hue, then saturation, then luminance, each over the eight ranges in order.
        let property = |offset: usize| -> [f64; unit::RANGE_COUNT] {
            std::array::from_fn(|range| values.number(FIELDS[offset + range]))
        };
        let (hue, saturation, luminance) = (
            property(0),
            property(unit::RANGE_COUNT),
            property(2 * unit::RANGE_COUNT),
        );
        let mixer: Arc<dyn PointwiseColor> = Arc::new(unit::Mixer::new(hue, saturation, luminance));
        Ok(Processing::Color(ColorOperation::new(vec![mixer])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{ActionInput, ToolModule};
    use serde_json::json;

    const SET_MIXER: &str = "set-mixer";
    const STAGE: Stage = Stage {
        width: 4,
        height: 4,
    };

    /// The words a history label and the recipe row use for each field, with its declared decimals,
    /// sign and unit. The rules that choose a label — a group reset, the module reset, a field count
    /// — are the shared field-patch rules the conformance suite proves for every module.
    #[test]
    fn each_field_is_named_by_its_own_history_words() {
        let module = MixerModule::new();
        for (parameters, expected) in [
            (json!({"red-hue": 20.0}), "Red hue +20"),
            (json!({"aqua-luminance": -15.0}), "Aqua luminance -15"),
            (
                json!({"magenta-saturation": -100.0}),
                "Magenta saturation -100",
            ),
        ] {
            let label = module.label(
                module.descriptor().action(SET_MIXER).unwrap(),
                &ActionInput {
                    action_id: SET_MIXER.to_owned(),
                    parameters: parameters.as_object().cloned().unwrap(),
                },
            );
            assert_eq!(label, expected, "{parameters}");
        }
    }

    #[test]
    fn compile_is_neutral_for_the_empty_payload_and_a_unit_otherwise() {
        let module = MixerModule::new();
        let neutral = module.compile(MIXER_EFFECT, 1, &json!({}), STAGE).unwrap();
        assert_eq!(
            neutral,
            Processing::Color(super::super::ColorOperation::neutral())
        );

        let coloured = module
            .compile(MIXER_EFFECT, 1, &json!({"red-hue": 20.0}), STAGE)
            .unwrap();
        match coloured {
            Processing::Color(operation) => assert_eq!(operation.len(), 1),
            other => panic!("expected a colour operation, got {other:?}"),
        }
    }
}
