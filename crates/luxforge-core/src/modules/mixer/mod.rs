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
//! order"). The layer compiles to the HSL unit, whose equations are frozen in
//! `docs/design/mixer-study.md` and implemented in [`unit::Mixer`], followed by the grading unit
//! (`docs/design/colour-grading.md`, implemented in [`grade::Grade`]) in the same unbroken colour
//! run; each is omitted when it would change nothing. This file owns only the field table, the
//! controls' rails and the compilation into those units.
mod grade;
mod unit;

/// The mixer's GPU programs, which [`super::GPU_PROGRAMS`] lists: the HSL unit's and the grading
/// unit's.
pub(crate) use grade::PROGRAM as GRADE_PROGRAM;
pub(crate) use unit::PROGRAM as MIXER_PROGRAM;

use super::{
    ColorOperation, EffectStage, NumberStyle, PointwiseColor, Processing, RailDecoration,
    WheelStyle,
    descriptor::title_case,
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
/// fields, listed among [`super::BUILTIN_FORMATS`]. A stored mixer layer at any other format is
/// refused as `incompatible` and never rewritten.
pub(super) const MIXER_EFFECT_FORMAT: u32 = 2;

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

/// One grading wheel: its id (`shadows`, as [`grade::WHEEL_NAMES`] spells it), which names its
/// individual view and its three fields; the label its wheel, its view and its history words
/// use; its `grade-<id>-hue`, `-saturation` and `-luminance` fields; and whether it is tonal, a
/// range Blending and Balance shape, which Global is not.
struct GradeWheel {
    id: &'static str,
    label: &'static str,
    hue: &'static str,
    saturation: &'static str,
    luminance: &'static str,
    tonal: bool,
}

/// A [`GradeWheel`] whose field names are derived from its id.
macro_rules! grade_wheel {
    ($id:literal, $label:literal, $tonal:literal) => {
        GradeWheel {
            id: $id,
            label: $label,
            hue: concat!("grade-", $id, "-hue"),
            saturation: concat!("grade-", $id, "-saturation"),
            luminance: concat!("grade-", $id, "-luminance"),
            tonal: $tonal,
        }
    };
}

const SHADOWS: GradeWheel = grade_wheel!("shadows", "Shadows", true);
const MIDTONES: GradeWheel = grade_wheel!("midtones", "Midtones", true);
const HIGHLIGHTS: GradeWheel = grade_wheel!("highlights", "Highlights", true);
const GLOBAL: GradeWheel = grade_wheel!("global", "Global", false);

/// The four wheels in [`grade::WHEEL_NAMES`] order: the one table the grading fields, the Grading
/// group's wheels and views and the grading values are all read from. Their fields follow the HSL
/// fields in the payload's declared order, wheel by wheel, then Blending and Balance.
const WHEELS: [GradeWheel; grade::WHEEL_COUNT] = [SHADOWS, MIDTONES, HIGHLIGHTS, GLOBAL];

const GRADE_BLENDING: &str = "grade-blending";
const GRADE_BALANCE: &str = "grade-balance";

impl GradeWheel {
    fn fields(&self) -> [&'static str; 3] {
        [self.hue, self.saturation, self.luminance]
    }
}

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
    let label = title_case(unit::RANGE_NAMES[range]);
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
                .id("hsl")
                .tabs()
                .subgroup(Group::new(HUE_GROUP, HSL_FIELDS[0..8].iter().copied()).id(HUE))
                .subgroup(
                    Group::new(SATURATION_GROUP, HSL_FIELDS[8..16].iter().copied()).id(SATURATION),
                )
                .subgroup(
                    Group::new(LUMINANCE_GROUP, HSL_FIELDS[16..24].iter().copied()).id(LUMINANCE),
                ),
        )
        .group(grading_group())
        .collapsed()
        .layout(crate::ModuleLayout::Tabs)
    }

    /// Only a layer with a field away from its default reaches here, unless the GPU shape asks for
    /// every unit; the shared field patch compiles an all-default one to no units.
    ///
    /// The HSL unit runs first and the grading unit second, in one unbroken colour run. Each is
    /// added only when it changes a pixel: HSL when any of its twenty-four fields is moved, grading
    /// when any saturation or luminance amount is ([`grade::Grading::is_active`]), so a layer
    /// holding only a dormant hue, Blending or Balance compiles to no units and an HSL-only layer
    /// renders exactly as it did before grading existed. In the GPU shape both are added, a neutral
    /// one as its identity, so a drag that leaves or returns to neutral keeps one program sequence
    /// (`CompileStage::gpu_shape`); no CPU compile asks for it.
    fn compile(&self, values: &Values<'_>, at: crate::CompileStage) -> Result<Processing, Error> {
        let every = at.gpu_shape;
        let mut units: Vec<Arc<dyn PointwiseColor>> = Vec::new();
        // HSL_FIELDS is hue, then saturation, then luminance, each over the eight ranges in order.
        let property = |offset: usize| -> [f64; unit::RANGE_COUNT] {
            std::array::from_fn(|range| values.number(HSL_FIELDS[offset + range]))
        };
        let (hue, saturation, luminance) = (
            property(0),
            property(unit::RANGE_COUNT),
            property(2 * unit::RANGE_COUNT),
        );
        let hsl_moved = HSL_FIELDS.iter().any(|name| values.number(name) != NEUTRAL);
        if every || hsl_moved {
            units.push(Arc::new(unit::Mixer::new(hue, saturation, luminance)));
        }
        let grading = grading(values);
        if every || grading.is_active() {
            units.push(Arc::new(grade::Grade::new(grading)));
        }
        Ok(Processing::Color(ColorOperation::new(units)))
    }
}

/// The Grading tab: one group owning the fourteen grading fields and declaring the four wheels,
/// with its reset (Blending back to 50), shown through presentation-only views. 3-way draws
/// Midtones above Shadows and Highlights in compact wheels; each range and Global has a view of
/// its own with its large wheel and exact hue and saturation. Blending and Balance follow every
/// tonal view and are absent from Global, which does not read them; a view never changes a value,
/// a reset scope or what a preset holds.
fn grading_group() -> Group {
    let fields = WHEELS
        .iter()
        .flat_map(GradeWheel::fields)
        .chain([GRADE_BLENDING, GRADE_BALANCE]);
    let three_way = [MIDTONES, SHADOWS, HIGHLIGHTS]
        .iter()
        .fold(View::new("three-way", "3-way"), |view, wheel| {
            view.wheel(wheel.label, WheelStyle::Compact)
        })
        .field(GRADE_BLENDING)
        .field(GRADE_BALANCE);
    WHEELS.iter().fold(
        Group::new(GRADING_GROUP, fields)
            .id("grading")
            .view(three_way),
        |group, wheel| {
            let view = View::new(wheel.id, wheel.label)
                .wheel(wheel.label, WheelStyle::Large)
                .field(wheel.hue)
                .field(wheel.saturation);
            let view = if wheel.tonal {
                view.field(GRADE_BLENDING).field(GRADE_BALANCE)
            } else {
                view
            };
            group
                .wheel(
                    Wheel::new(wheel.label, wheel.hue, wheel.saturation).luminance(wheel.luminance),
                )
                .view(view)
        },
    )
}

/// The fourteen grading values a payload holds, defaults filled.
fn grading(values: &Values<'_>) -> grade::Grading {
    grade::Grading {
        wheels: WHEELS.map(|wheel| grade::Wheel {
            hue: values.number(wheel.hue),
            saturation: values.number(wheel.saturation),
            luminance: values.number(wheel.luminance),
        }),
        blending: values.number(GRADE_BLENDING),
        balance: values.number(GRADE_BALANCE),
    }
}

/// The black-to-white rail every grading luminance slider draws.
fn grade_luminance_rail() -> RailDecoration {
    RailDecoration::Gradient {
        stops: vec![[0, 0, 0], MID_GREY, [255, 255, 255]],
    }
}

/// The fourteen grading fields, wheel by wheel in [`WHEELS`] order, then Blending and Balance. Hue
/// is a direction on the RGB colour wheel, 0..360 with 0 and 360 the same direction, kept as
/// stored even at zero saturation; saturation is a one-sided strength, 0..100; luminance is
/// signed, -100..100; Blending defaults to 50 and Balance to 0.
fn grade_fields() -> Vec<Field> {
    let mut fields = Vec::with_capacity(3 * WHEELS.len() + 2);
    for wheel in &WHEELS {
        let label = wheel.label;
        let scope = if wheel.tonal {
            format!("the {} range", wheel.id)
        } else {
            "every tone, independent of Blending and Balance".to_owned()
        };
        fields.push(
            Field::slider(
                wheel.hue,
                format!("{label} hue"),
                format!(
                    "the direction of the {label} tint on the RGB colour wheel (red 0, green 120, blue 240), in degrees; 0 and 360 are the same direction. Kept as a setting at zero saturation, where it changes no pixel"
                ),
            )
            .range(0.0, 360.0)
            .control(FieldControl::Number(NumberStyle::Field)),
        );
        fields.push(
            Field::slider(
                wheel.saturation,
                format!("{label} saturation"),
                format!(
                    "the strength of the {label} tint over {scope}: 0 adds no colour, 100 the strongest tint; greys are tinted too, and black and white stay neutral unless the luminance treatment moves them"
                ),
            )
            .range(0.0, 100.0)
            .control(FieldControl::Number(NumberStyle::Field)),
        );
        fields.push(
            Field::slider(
                wheel.luminance,
                format!("{label} luminance"),
                format!(
                    "brightens or darkens {scope} through a bounded gamma on Oklab L; works without any tint"
                ),
            )
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
        .default(grade::DEFAULT_BLENDING),
    );
    fields.push(
        Field::slider(
            GRADE_BALANCE,
            "Balance",
            "moves the split between the tonal ranges: negative extends Shadows, positive extends Highlights; does not affect Global",
        )
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

    /// The words a history label and the recipe row use for each field, with its declared decimals
    /// and sign: an HSL field by its range and property, a grading field by its wheel and
    /// property, Blending and Balance by themselves; and the documented wheel and group labels.
    /// The rules that choose a label are the shared field-patch rules.
    #[test]
    fn each_field_and_wheel_is_named_by_its_own_history_words() {
        let module = MixerModule::new();
        for (parameters, expected) in [
            (json!({"red-hue": 20.0}), "Red hue +20"),
            (json!({"aqua-luminance": -15.0}), "Aqua luminance -15"),
            (
                json!({"magenta-saturation": -100.0}),
                "Magenta saturation -100",
            ),
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
            (
                json!({"grade-shadows-hue": 120.0, "grade-shadows-saturation": 40.0}),
                "Shadows tint",
            ),
            (
                json!({"grade-midtones-hue": 30.0, "grade-midtones-saturation": 20.0,
                       "grade-midtones-luminance": 10.0}),
                "Midtones",
            ),
            (
                json!({"grade-global-hue": 0.0, "grade-global-saturation": 0.0,
                       "grade-global-luminance": 0.0}),
                "Reset Global",
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

    fn units(payload: serde_json::Value, gpu_shape: bool) -> Vec<String> {
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
            Processing::Color(operation) => operation
                .units()
                .iter()
                .map(|unit| {
                    let description = unit.describe();
                    description.split('(').next().unwrap_or_default().to_owned()
                })
                .collect(),
            other => panic!("expected a colour operation, got {other:?}"),
        }
    }

    /// HSL and grading compile to their own units, HSL first, each only when it changes a pixel: a
    /// dormant hue, Blending or Balance compiles to nothing, and the GPU shape holds both.
    #[test]
    fn each_unit_compiles_only_when_it_changes_a_pixel() {
        assert_eq!(units(json!({"red-hue": 20.0}), false), ["mixer"]);
        assert_eq!(
            units(json!({"grade-midtones-saturation": 20.0}), false),
            ["grade"]
        );
        assert_eq!(
            units(json!({"grade-shadows-luminance": -5.0}), false),
            ["grade"]
        );
        assert_eq!(
            units(
                json!({"red-hue": 20.0, "grade-global-saturation": 5.0}),
                false
            ),
            ["mixer", "grade"]
        );
        assert!(
            units(
                json!({"grade-shadows-hue": 200.0, "grade-blending": 0.0, "grade-balance": 40.0}),
                false
            )
            .is_empty()
        );
        assert_eq!(units(json!({}), true), ["mixer", "grade"]);
        assert_eq!(
            units(json!({"grade-shadows-hue": 200.0}), true),
            ["mixer", "grade"]
        );
    }

    /// The wheel table is in the grading unit's wheel order, each label is its id read as a
    /// word, and only Global is not tonal.
    #[test]
    fn the_wheel_table_follows_the_grading_units_wheel_order() {
        assert_eq!(WHEELS.map(|wheel| wheel.id), grade::WHEEL_NAMES);
        assert!(
            WHEELS
                .iter()
                .all(|wheel| title_case(wheel.id) == wheel.label)
        );
        assert_eq!(WHEELS.map(|wheel| wheel.tonal), [true, true, true, false]);
    }
}
