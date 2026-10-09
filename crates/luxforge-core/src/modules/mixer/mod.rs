//! The colour mixer module: one colour-stage layer holding hue, saturation and luminance for each
//! of eight colour ranges, and the colour grading of Shadows, Midtones, Highlights and Global with
//! Blending and Balance, edited by one field-patch action.
//!
//! This is a field-patch module (`docs/design/modules-and-api.md`'s field-patch contract, shared in
//! [`super::field_patch`]): a payload is a JSON object whose keys are the implemented parameter
//! names, a missing key is that field's default, and the canonical all-default payload is the empty
//! object `{}`. The module owns exactly one layer of `luxforge.mixer.hsl` per target, at its own
//! format marker [`MIXER_EFFECT_FORMAT`], declared order 10 so a mixer layer always follows the
//! Basic layer in the colour run (`docs/design/presence-mixer-vignette.md`, "Placement and stage
//! order"). The layer compiles to one colour unit, [`unit::Mixer`]: one Oklab round trip around
//! the HSL stage, whose equations are frozen in `docs/design/mixer-study.md` and implemented in
//! [`unit::Hsl`], followed by the grading stage (`docs/design/colour-grading.md`, implemented in
//! [`grade::Grade`]); each stage is omitted when it would change nothing. This file owns only the
//! field table, the controls' rails and the compilation into that unit.
mod grade;
mod unit;

/// The mixer's GPU program, which [`super::GPU_PROGRAMS`] lists: its one colour unit's, HSL and
/// grading together.
pub(crate) use unit::PROGRAM as MIXER_PROGRAM;

use super::{
    ColorOperation, EffectStage, NumberStyle, PointwiseColor, Processing, RailDecoration,
    field_patch::{
        Field, FieldControl, FieldPatch, FieldPatchModule, Group, Spec, Values, View, Wheel,
    },
};
use crate::Error;
use std::sync::Arc;

/// The colour mixer's one effect: hue, saturation and luminance for the eight colour ranges and the
/// colour grading, declared order 10 so a mixer layer always follows the Basic layer in the colour
/// run. The identity keeps its historical `hsl` spelling; grading is part of the same layer.
pub const MIXER_EFFECT: &str = "luxforge.mixer.hsl";

/// The mixer effect's current payload format: 2 since the payload gained the fourteen grading
/// fields. A stored mixer layer at any other format is refused as `incompatible` and never
/// rewritten.
pub const MIXER_EFFECT_FORMAT: u32 = 2;

const HUE: &str = "hue";
const SATURATION: &str = "saturation";
const LUMINANCE: &str = "luminance";

/// Every HSL field: the hue group, then saturation, then luminance, each in range order red,
/// orange, yellow, green, aqua, blue, purple, magenta. They lead the payload's declared order, in
/// the order [`unit::Mixer::new`] expects its three eight-element arrays in.
///
/// Names are `<range>-<property>`, hyphen-separated: [`crate::modules::valid_name`] is the shared
/// identity rule every action and parameter name is checked against (lowercase words joined by
/// `-`), so `red-hue` is what actually registers.
const HSL_FIELDS: [&str; 24] = [
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

/// The grading fields, after the HSL fields in the payload's declared order: hue, saturation and
/// luminance for Shadows, Midtones, Highlights and Global, in [`grade::WHEEL_NAMES`] order, then
/// Blending and Balance.
const GRADE_FIELDS: [&str; 14] = [
    "grade-shadows-hue",
    "grade-shadows-saturation",
    "grade-shadows-luminance",
    "grade-midtones-hue",
    "grade-midtones-saturation",
    "grade-midtones-luminance",
    "grade-highlights-hue",
    "grade-highlights-saturation",
    "grade-highlights-luminance",
    "grade-global-hue",
    "grade-global-saturation",
    "grade-global-luminance",
    GRADE_BLENDING,
    GRADE_BALANCE,
];

const GRADE_BLENDING: &str = "grade-blending";
const GRADE_BALANCE: &str = "grade-balance";

/// The group label a patch that returns every one of a property's eight fields to neutral takes in
/// history, and the label the group's own control carries.
const HUE_GROUP: &str = "Hue";
const SATURATION_GROUP: &str = "Saturation";
const LUMINANCE_GROUP: &str = "Luminance";
const GRADING_GROUP: &str = "Grading";
const HSL_GROUP: &str = "HSL";

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
fn hsl_field(name: &'static str) -> Field {
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
            "Hue, saturation and luminance by range, and colour grading",
            MIXER_EFFECT,
            EffectStage::Color,
        )
        .format(MIXER_EFFECT_FORMAT)
        .order(10)
        .maskable()
        .set_notes("merges the named mixer fields into the stack's one Colour mixer layer, which the host places after Basic by declared order on the first value away from its default and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op. A grading hue, Blending or Balance with every grading saturation and luminance at 0 is kept as a setting but changes no pixel")
        .reset_notes("returns the stack's one Colour mixer layer to its all-default payload, HSL and grading together (Blending 50), keeping its identity and position; a no-op without one and when it is already all default")
        .fields(HSL_FIELDS.map(hsl_field))
        .fields(grade_fields())
        // HSL and Grading are the band's two tabs, each its own reset and preset-capture scope.
        // HSL's Hue, Saturation and Luminance are three parallel subgroups over the same eight
        // ranges, drawn as nested tabs and each resetting its own eight fields.
        .group(
            Group::new(HSL_GROUP, [])
                .tabs()
                .subgroup(Group::new(HUE_GROUP, HSL_FIELDS[0..8].iter().copied()))
                .subgroup(Group::new(SATURATION_GROUP, HSL_FIELDS[8..16].iter().copied()))
                .subgroup(Group::new(LUMINANCE_GROUP, HSL_FIELDS[16..24].iter().copied())),
        )
        .group(grading_group())
        .collapsed()
        .layout(crate::ModuleLayout::Tabs)
    }

    /// Only a layer with a field away from its default reaches here, unless the GPU shape asks for
    /// every unit; the shared field patch compiles an all-default one to no units.
    ///
    /// One colour unit, one Oklab round trip, with the HSL stage first and the grading stage
    /// second. Each stage is present only when it changes a pixel: HSL when any of its twenty-four
    /// fields is moved, grading when any saturation or luminance amount is
    /// ([`grade::Grading::is_active`]), so a layer holding only a dormant hue, Blending or Balance
    /// compiles to no units and an HSL-only layer renders exactly as it did before grading
    /// existed. In the GPU shape the unit is compiled even with neither stage, as the identity, so
    /// a drag that leaves or returns to neutral keeps one program sequence
    /// (`CompileStage::gpu_shape`); no CPU compile asks for it.
    fn compile(&self, values: &Values<'_>, at: crate::CompileStage) -> Result<Processing, Error> {
        // HSL_FIELDS is hue, then saturation, then luminance, each over the eight ranges in order.
        let property = |offset: usize| -> [f64; unit::RANGE_COUNT] {
            std::array::from_fn(|range| values.number(HSL_FIELDS[offset + range]))
        };
        let (hue, saturation, luminance) = (
            property(0),
            property(unit::RANGE_COUNT),
            property(2 * unit::RANGE_COUNT),
        );
        let hsl_moved = [hue, saturation, luminance]
            .iter()
            .flatten()
            .any(|value| *value != NEUTRAL);
        let hsl = hsl_moved.then(|| unit::Hsl::new(hue, saturation, luminance));
        let grading = grading(values);
        let grade = grading.is_active().then(|| grade::Grade::new(grading));
        let mut units: Vec<Arc<dyn PointwiseColor>> = Vec::new();
        if at.gpu_shape || hsl.is_some() || grade.is_some() {
            units.push(Arc::new(unit::Mixer::new(hsl, grade)));
        }
        Ok(Processing::Color(ColorOperation::new(units)))
    }
}

/// One wheel's three fields: hue, saturation and luminance.
fn wheel_fields(wheel: usize) -> [&'static str; 3] {
    [
        GRADE_FIELDS[3 * wheel],
        GRADE_FIELDS[3 * wheel + 1],
        GRADE_FIELDS[3 * wheel + 2],
    ]
}

/// The wheel labels and history words, in [`grade::WHEEL_NAMES`] order.
const WHEEL_LABELS: [&str; grade::WHEEL_COUNT] = ["Shadows", "Midtones", "Highlights", "Global"];
const WHEEL_TINTS: [&str; grade::WHEEL_COUNT] = [
    "Shadows tint",
    "Midtones tint",
    "Highlights tint",
    "Global tint",
];

/// One wheel over its hue and saturation, with its luminance as the wheel's rail: the reset of
/// exactly its three fields, and one history entry, `<Range> tint`, for a gesture moving both.
fn grade_wheel(wheel: usize) -> Wheel {
    let [hue, saturation, luminance] = wheel_fields(wheel);
    Wheel::new(WHEEL_LABELS[wheel], hue, saturation)
        .luminance(luminance)
        .history(WHEEL_TINTS[wheel])
}

/// The Grading tab: one group owning the fourteen grading fields, its reset (Blending back to 50),
/// shown through presentation-only views. 3-way draws Midtones above Shadows and Highlights in
/// compact wheels; each range and Global has a view of its own with a large wheel and its exact
/// hue and saturation. Blending and Balance follow every tonal view and are absent from Global,
/// which does not read them; a view never changes a value, a reset scope or what a preset holds.
fn grading_group() -> Group {
    const SHADOWS: usize = 0;
    const MIDTONES: usize = 1;
    const HIGHLIGHTS: usize = 2;
    const GLOBAL: usize = 3;
    let three_way = View::new("3-way")
        .wheel(grade_wheel(MIDTONES))
        .wheel(grade_wheel(SHADOWS))
        .wheel(grade_wheel(HIGHLIGHTS))
        .field(GRADE_BLENDING)
        .field(GRADE_BALANCE);
    let single = |wheel: usize, tonal: bool| {
        let [hue, saturation, _] = wheel_fields(wheel);
        let view = View::new(WHEEL_LABELS[wheel])
            .wheel(grade_wheel(wheel).large())
            .field(hue)
            .field(saturation);
        if tonal {
            view.field(GRADE_BLENDING).field(GRADE_BALANCE)
        } else {
            view
        }
    };
    Group::new(GRADING_GROUP, GRADE_FIELDS)
        .tabs()
        .view(three_way)
        .view(single(SHADOWS, true))
        .view(single(MIDTONES, true))
        .view(single(HIGHLIGHTS, true))
        .view(single(GLOBAL, false))
}

/// The fourteen grading values a payload holds, defaults filled.
fn grading(values: &Values<'_>) -> grade::Grading {
    grade::Grading {
        wheels: std::array::from_fn(|wheel| {
            let field = |property: usize| values.number(GRADE_FIELDS[3 * wheel + property]);
            grade::WheelValues {
                hue: field(0),
                saturation: field(1),
                luminance: field(2),
            }
        }),
        blending: values.number(GRADE_BLENDING),
        balance: values.number(GRADE_BALANCE),
    }
}

/// The capitalized wheel name a control label and a history label both use, e.g. `Shadows`.
fn wheel_label(wheel: usize) -> String {
    let name = grade::WHEEL_NAMES[wheel];
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The black-to-white rail every grading luminance slider draws.
fn grade_luminance_rail() -> RailDecoration {
    RailDecoration::Gradient {
        stops: vec![[0, 0, 0], MID_GREY, [255, 255, 255]],
    }
}

/// The fourteen grading fields, in [`GRADE_FIELDS`] order. Hue is a direction on the RGB colour
/// wheel, 0..360 with 0 and 360 the same direction, kept as stored even at zero saturation;
/// saturation is a one-sided strength, 0..100; luminance is signed, -100..100; Blending defaults
/// to 50 and Balance to 0.
fn grade_fields() -> Vec<Field> {
    let mut fields = Vec::with_capacity(GRADE_FIELDS.len());
    for wheel in 0..grade::WHEEL_COUNT {
        let label = wheel_label(wheel);
        let names = &GRADE_FIELDS[3 * wheel..3 * wheel + 3];
        let scope = if wheel == 3 {
            "every tone, independent of Blending and Balance".to_owned()
        } else {
            format!("the {} range", label.to_lowercase())
        };
        fields.push(
            Field::slider(
                names[0],
                format!("{label} hue"),
                format!(
                    "the direction of the {label} tint on the RGB colour wheel (red 0, green 120, blue 240), in degrees; 0 and 360 are the same direction. Kept as a setting at zero saturation, where it changes no pixel"
                ),
            )
            .range(0.0, 360.0)
            .history(format!("{label} hue"))
            .control(FieldControl::Number(NumberStyle::Field)),
        );
        fields.push(
            Field::slider(
                names[1],
                format!("{label} saturation"),
                format!(
                    "the strength of the {label} tint over {scope}: 0 adds no colour, 100 the strongest tint; greys are tinted too, and black and white stay neutral unless the luminance treatment moves them"
                ),
            )
            .range(0.0, 100.0)
            .history(format!("{label} saturation"))
            .control(FieldControl::Number(NumberStyle::Field)),
        );
        fields.push(
            Field::slider(
                names[2],
                format!("{label} luminance"),
                format!(
                    "brightens or darkens {scope} through a bounded gamma on Oklab L; works without any tint"
                ),
            )
            .history(format!("{label} luminance"))
            .zero(NEUTRAL)
            .rail(grade_luminance_rail()),
        );
    }
    fields.push(
        Field::slider(
            GRADE_BLENDING,
            "Blending",
            "how much the Shadows, Midtones and Highlights ranges overlap: 0 is the narrowest smooth transition, 100 the widest; does not affect Global",
        )
        .range(0.0, 100.0)
        .default(grade::DEFAULT_BLENDING)
        .history("Blending"),
    );
    fields.push(
        Field::slider(
            GRADE_BALANCE,
            "Balance",
            "moves the split between the tonal ranges: negative extends Shadows, positive extends Highlights; does not affect Global",
        )
        .history("Balance")
        .zero(NEUTRAL),
    );
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stage;
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

    fn compiled(payload: serde_json::Value, gpu_shape: bool) -> Vec<Arc<dyn PointwiseColor>> {
        let module = MixerModule::new();
        match module
            .compile(
                MIXER_EFFECT,
                MIXER_EFFECT_FORMAT,
                &payload,
                crate::CompileStage::exact(STAGE).shaped(gpu_shape),
            )
            .unwrap()
        {
            Processing::Color(operation) => operation.units().to_vec(),
            other => panic!("expected a colour operation, got {other:?}"),
        }
    }

    fn units(payload: serde_json::Value, gpu_shape: bool) -> Vec<String> {
        compiled(payload, gpu_shape)
            .iter()
            .map(|unit| unit.describe())
            .collect()
    }

    fn identity(payload: serde_json::Value, gpu_shape: bool) -> crate::OperationIdentity {
        let units = compiled(payload, gpu_shape);
        assert_eq!(units.len(), 1);
        units[0].identity()
    }

    /// A layer compiles to one unit holding each stage only when it changes a pixel, HSL first: a
    /// dormant hue, Blending or Balance compiles to nothing, and the GPU shape holds the unit with
    /// neither stage.
    #[test]
    fn each_stage_compiles_only_when_it_changes_a_pixel() {
        assert_eq!(
            units(json!({"red-hue": 20.0}), false),
            ["mixer(red-hue:+20)"]
        );
        assert_eq!(
            units(json!({"grade-midtones-saturation": 20.0}), false),
            ["mixer(grade-midtones-saturation:+20)"]
        );
        assert_eq!(
            units(json!({"grade-shadows-luminance": -5.0}), false),
            ["mixer(grade-shadows-luminance:-5)"]
        );
        assert_eq!(
            units(
                json!({"red-hue": 20.0, "grade-global-saturation": 5.0}),
                false
            ),
            ["mixer(red-hue:+20, grade-global-saturation:+5)"]
        );
        assert!(
            units(
                json!({"grade-shadows-hue": 200.0, "grade-blending": 0.0, "grade-balance": 40.0}),
                false
            )
            .is_empty()
        );
        assert_eq!(units(json!({}), true), ["mixer(neutral)"]);
        assert_eq!(
            units(json!({"grade-shadows-hue": 200.0}), true),
            ["mixer(neutral)"]
        );
    }

    /// The compiled unit's identity is its stage flags and coefficients: a dormant grading edit
    /// changes none of it, while a Blending the active grading reads does.
    #[test]
    fn a_dormant_edit_keeps_the_identity() {
        let with = |base: &serde_json::Value, extra: serde_json::Value| {
            let mut payload = base.as_object().unwrap().clone();
            payload.extend(extra.as_object().unwrap().clone());
            serde_json::Value::Object(payload)
        };
        let dormant =
            json!({"grade-shadows-hue": 200.0, "grade-blending": 10.0, "grade-balance": 40.0});
        let hsl = json!({"red-hue": 20.0});
        for gpu_shape in [false, true] {
            assert_eq!(
                identity(with(&hsl, dormant.clone()), gpu_shape),
                identity(hsl.clone(), gpu_shape)
            );
        }
        assert_eq!(identity(dormant, true), identity(json!({}), true));
        let grade = json!({"grade-midtones-saturation": 20.0});
        assert_eq!(
            identity(with(&grade, json!({"grade-shadows-hue": 200.0})), false),
            identity(grade.clone(), false)
        );
        assert_ne!(
            identity(with(&grade, json!({"grade-blending": 10.0})), false),
            identity(grade, false)
        );
    }

    /// A layer at the shared format the mixer used before grading is refused, never read.
    #[test]
    fn a_layer_at_the_previous_format_is_refused() {
        let module = MixerModule::new();
        let error = module
            .validate_payload(
                MIXER_EFFECT,
                crate::EFFECT_FORMAT,
                &json!({"red-hue": 20.0}),
            )
            .unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Incompatible);
        assert_eq!(module.descriptor().effects[0].format, MIXER_EFFECT_FORMAT);
    }

    /// Grading fields are named by their wheel and property, Blending and Balance by themselves.
    #[test]
    fn grading_fields_are_named_by_their_own_history_words() {
        let module = MixerModule::new();
        for (parameters, expected) in [
            (json!({"grade-shadows-hue": 210.0}), "Shadows hue 210"),
            (
                json!({"grade-global-saturation": 35.0}),
                "Global saturation 35",
            ),
            (
                json!({"grade-highlights-luminance": -20.0}),
                "Highlights luminance -20",
            ),
            (json!({"grade-blending": 70.0}), "Blending 70"),
            (json!({"grade-balance": 15.0}), "Balance +15"),
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
}
