//! Every variant's XMP read back through Luxforge's own preset reader
//! ([`luxforge_core::inspect_preset`]), which must find exactly what the manifest records.
//!
//! The reader reports every setting the file holds in one of its lists with the text as written.
//! A read-back passes when nothing is refused; the settings it reports are exactly the settings
//! written, each with its written text; each value transfer lands on its Luxforge control at the
//! same number (a point curve at its points over 255), or, for a setting the Lightroom alignment
//! has calibrated, where its frozen calibration puts it, refused at its shortfall; and anything
//! reported unsupported is a
//! setting this rig compares through its own counterpart, because the importer does not map it
//! (Detail, crop, perspective, lens profiles and gradient corrections). A gradient correction is
//! reported as a summary, so its geometry is read back by this rig's own parse of the same text.
//! Grain at Amount 0 imports no grain: the reader may list its three settings as neutral rather
//! than transfer them.
use super::settings::{Field, Fields, corrections};
use luxforge_core::{ModuleRegistry, inspect_preset};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// How a Lightroom setting reaches the Luxforge control it is compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// The importer's value transfer: the same number on the control.
    Transfer,
    /// The importer's conversion: RAW white balance through the white it names.
    Converted,
    /// The importer does not map it; the rig names the control a measurement compares it with.
    NotMapped,
}

impl How {
    pub fn name(self) -> &'static str {
        match self {
            Self::Transfer => "value transfer",
            Self::Converted => "converted by the importer",
            Self::NotMapped => "not mapped by the importer",
        }
    }
}

const BASIC: &str = "set-basic";
const PRESENCE: &str = "set-presence";
const MIXER: &str = "set-mixer";
const VIGNETTE: &str = "set-vignette";
const DETAIL: &str = "set-detail";
const GRAIN: &str = "set-grain";

/// Whether `name` is one of grain's settings written at a neutral Amount, which the importer
/// imports as no grain at all and so may list as neutral rather than transfer.
fn neutral_grain(name: &str, fields: &Fields) -> bool {
    matches!(name, "GrainAmount" | "GrainSize" | "GrainFrequency")
        && matches!(
            fields.get("GrainAmount"),
            Some(Field::Number { value, .. }) if *value == 0.0
        )
}

/// The Luxforge control each Lightroom setting the rounds write is compared with, and how it gets
/// there; `None` for a setting with no counterpart, which the rounds hold at its default.
pub fn counterpart(name: &str) -> Option<(&'static str, &'static str, How)> {
    use How::{Converted, NotMapped, Transfer};
    let transfer = |action, field| Some((action, field, Transfer));
    match name {
        "Exposure2012" => transfer(BASIC, "exposure"),
        "Contrast2012" => transfer(BASIC, "contrast"),
        "Highlights2012" => transfer(BASIC, "highlights"),
        "Shadows2012" => transfer(BASIC, "shadows"),
        "Whites2012" => transfer(BASIC, "whites"),
        "Blacks2012" => transfer(BASIC, "blacks"),
        "Vibrance" => transfer(BASIC, "vibrance"),
        "Saturation" => transfer(BASIC, "saturation"),
        "IncrementalTemperature" => transfer(BASIC, "temperature"),
        "IncrementalTint" => transfer(BASIC, "tint"),
        "Texture" => transfer(PRESENCE, "texture"),
        "Clarity2012" => transfer(PRESENCE, "clarity"),
        "Dehaze" => transfer(PRESENCE, "dehaze"),
        "ToneCurvePV2012" => transfer("set-curve", "luminance"),
        "PostCropVignetteAmount" => transfer(VIGNETTE, "amount"),
        "PostCropVignetteMidpoint" => transfer(VIGNETTE, "midpoint"),
        "PostCropVignetteRoundness" => transfer(VIGNETTE, "roundness"),
        "PostCropVignetteFeather" => transfer(VIGNETTE, "feather"),
        "GrainAmount" => transfer(GRAIN, "amount"),
        "GrainSize" => transfer(GRAIN, "size"),
        "GrainFrequency" => transfer(GRAIN, "roughness"),
        "SplitToningShadowHue" => transfer(MIXER, "grade-shadows-hue"),
        "SplitToningShadowSaturation" => transfer(MIXER, "grade-shadows-saturation"),
        "SplitToningHighlightHue" => transfer(MIXER, "grade-highlights-hue"),
        "SplitToningHighlightSaturation" => transfer(MIXER, "grade-highlights-saturation"),
        "SplitToningBalance" => transfer(MIXER, "grade-balance"),
        "ColorGradeMidtoneHue" => transfer(MIXER, "grade-midtones-hue"),
        "ColorGradeMidtoneSat" => transfer(MIXER, "grade-midtones-saturation"),
        "ColorGradeGlobalHue" => transfer(MIXER, "grade-global-hue"),
        "ColorGradeGlobalSat" => transfer(MIXER, "grade-global-saturation"),
        "ColorGradeShadowLum" => transfer(MIXER, "grade-shadows-luminance"),
        "ColorGradeMidtoneLum" => transfer(MIXER, "grade-midtones-luminance"),
        "ColorGradeHighlightLum" => transfer(MIXER, "grade-highlights-luminance"),
        "ColorGradeGlobalLum" => transfer(MIXER, "grade-global-luminance"),
        "ColorGradeBlending" => transfer(MIXER, "grade-blending"),
        "WhiteBalance" => Some(("set-raw", "white-balance", Converted)),
        "Temperature" => Some(("set-raw", "temperature", Converted)),
        "Tint" => Some(("set-raw", "tint", Converted)),
        "Sharpness" => Some((DETAIL, "sharpening", NotMapped)),
        "SharpenRadius" => Some((DETAIL, "radius", NotMapped)),
        "SharpenDetail" => Some((DETAIL, "sharpen-detail", NotMapped)),
        "SharpenEdgeMasking" => Some((DETAIL, "sharpen-masking", NotMapped)),
        "LuminanceSmoothing" => Some((DETAIL, "luminance", NotMapped)),
        "LuminanceNoiseReductionDetail" => Some((DETAIL, "luminance-detail", NotMapped)),
        "ColorNoiseReduction" => Some((DETAIL, "colour", NotMapped)),
        "ColorNoiseReductionDetail" => Some((DETAIL, "colour-detail", NotMapped)),
        "PerspectiveVertical" => Some(("set-perspective", "vertical", NotMapped)),
        "PerspectiveHorizontal" => Some(("set-perspective", "horizontal", NotMapped)),
        "CropAngle" => Some(("crop", "angle", NotMapped)),
        "CropLeft" => Some(("crop", "x", NotMapped)),
        "CropTop" => Some(("crop", "y", NotMapped)),
        "CropRight" => Some(("crop", "width", NotMapped)),
        "CropBottom" => Some(("crop", "height", NotMapped)),
        "LensProfileEnable" => Some(("select-lens-profile", "profile", NotMapped)),
        _ => MIXER_FIELDS
            .iter()
            .find(|(setting, _)| *setting == name)
            .map(|(_, field)| (MIXER, *field, Transfer)),
    }
}

/// The mixer's Lightroom settings and the `set-mixer` fields they transfer to.
const MIXER_FIELDS: [(&str, &str); 24] = [
    ("HueAdjustmentRed", "red-hue"),
    ("HueAdjustmentOrange", "orange-hue"),
    ("HueAdjustmentYellow", "yellow-hue"),
    ("HueAdjustmentGreen", "green-hue"),
    ("HueAdjustmentAqua", "aqua-hue"),
    ("HueAdjustmentBlue", "blue-hue"),
    ("HueAdjustmentPurple", "purple-hue"),
    ("HueAdjustmentMagenta", "magenta-hue"),
    ("SaturationAdjustmentRed", "red-saturation"),
    ("SaturationAdjustmentOrange", "orange-saturation"),
    ("SaturationAdjustmentYellow", "yellow-saturation"),
    ("SaturationAdjustmentGreen", "green-saturation"),
    ("SaturationAdjustmentAqua", "aqua-saturation"),
    ("SaturationAdjustmentBlue", "blue-saturation"),
    ("SaturationAdjustmentPurple", "purple-saturation"),
    ("SaturationAdjustmentMagenta", "magenta-saturation"),
    ("LuminanceAdjustmentRed", "red-luminance"),
    ("LuminanceAdjustmentOrange", "orange-luminance"),
    ("LuminanceAdjustmentYellow", "yellow-luminance"),
    ("LuminanceAdjustmentGreen", "green-luminance"),
    ("LuminanceAdjustmentAqua", "aqua-luminance"),
    ("LuminanceAdjustmentBlue", "blue-luminance"),
    ("LuminanceAdjustmentPurple", "purple-luminance"),
    ("LuminanceAdjustmentMagenta", "magenta-luminance"),
];

/// The settings the reader may report unsupported in a round: those the importer does not map
/// and the rig compares through its own counterpart, their qualifiers, and gradient corrections.
const UNMAPPED: [&str; 24] = [
    "Sharpness",
    "SharpenRadius",
    "SharpenDetail",
    "SharpenEdgeMasking",
    "LuminanceSmoothing",
    "LuminanceNoiseReductionDetail",
    "LuminanceNoiseReductionContrast",
    "ColorNoiseReduction",
    "ColorNoiseReductionDetail",
    "ColorNoiseReductionSmoothness",
    "PerspectiveVertical",
    "PerspectiveHorizontal",
    "HasCrop",
    "CropTop",
    "CropLeft",
    "CropBottom",
    "CropRight",
    "CropAngle",
    "LensProfileEnable",
    "LensProfileSetup",
    "LensProfileDistortionScale",
    "LensProfileVignettingScale",
    "GradientBasedCorrections",
    "CircularGradientBasedCorrections",
];

/// What the reader made of one variant: its settings set, and the settings it reported
/// unsupported, which the rig compares through its own counterparts.
#[derive(Debug, PartialEq)]
pub struct Readback {
    pub settings: Map<String, Value>,
    pub unsupported: Vec<String>,
    /// RAW white-balance settings the importer refuses as out of Luxforge's range, with why.
    pub refused: Vec<Value>,
}

fn same_number(a: &Value, b: f64) -> bool {
    a.as_f64() == Some(b)
}

/// The value a transfer must land at: the written number, or a curve's points over 255.
fn expected(field: &Field) -> Option<Value> {
    match field {
        Field::Number { value, .. } => Some(json!(value)),
        Field::Curve(points) => Some(json!(
            points
                .iter()
                .map(|[x, y]| [f64::from(*x) / 255.0, f64::from(*y) / 255.0])
                .collect::<Vec<_>>()
        )),
        _ => None,
    }
}

fn same_value(read: &Value, expected: &Value) -> bool {
    match (read, expected) {
        (Value::Array(read), Value::Array(expected)) => {
            read.len() == expected.len() && read.iter().zip(expected).all(|(r, e)| same_value(r, e))
        }
        (read, Value::Number(expected)) => expected.as_f64().is_some_and(|e| same_number(read, e)),
        (read, expected) => read == expected,
    }
}

/// Read `xmp` back through the preset reader and check it against `fields`, the settings it was
/// written from.
pub fn read_back(
    fields: &Fields,
    xmp: &str,
    registry: &ModuleRegistry,
) -> Result<Readback, String> {
    let read = inspect_preset(xmp, None, registry).map_err(|error| error.to_string())?;
    let report = &read.report;
    // The refusals a round expects, each a range shortfall to measure: a RAW white-balance pair
    // whose white lies outside the Kelvin and tint range Luxforge's development holds, with the
    // Custom white balance that names it, and a calibrated value at or past its calibration's
    // shortfall.
    let shortfall = |entry: &luxforge_core::ReportedSetting| {
        let reason = entry.reason.as_deref().unwrap_or_default();
        match entry.setting.as_str() {
            "Temperature" | "Tint" => reason.starts_with("out-of-range"),
            "WhiteBalance" => reason.contains("Temperature and Tint are refused"),
            name => fields
                .get(name)
                .and_then(|field| match field {
                    Field::Number { value, .. } => {
                        luxforge_core::lightroom_calibration(name, *value)
                    }
                    _ => None,
                })
                .is_some_and(|converted| {
                    converted.as_ref().err().map(String::as_str) == Some(reason)
                }),
        }
    };
    if let Some(refused) = report.refused.iter().find(|entry| !shortfall(entry)) {
        return Err(format!(
            "the reader refused {} = {} ({})",
            refused.setting,
            refused.value,
            refused.reason.as_deref().unwrap_or("no reason")
        ));
    }
    if !report.derived.is_empty() {
        return Err("the reader derived settings the file does not hold".into());
    }
    // Every reported setting by name: its list and its text as written. A mapped setting may
    // land on several fields, each entry naming it with the same text.
    let mut reported: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for mapped in &report.mapped {
        let entry = ("mapped", mapped.value.as_str());
        match reported.insert(&mapped.setting, entry) {
            Some(existing) if existing != entry => {
                return Err(format!("{} is reported twice, differently", mapped.setting));
            }
            _ => {}
        }
    }
    for (list, entries) in [
        ("neutral", &report.neutral),
        ("unsupported", &report.unsupported),
        ("refused", &report.refused),
    ] {
        for entry in entries {
            if reported.contains_key(entry.setting.as_str()) {
                return Err(format!("{} is reported in two lists", entry.setting));
            }
            reported.insert(&entry.setting, (list, &entry.value));
        }
    }
    let written: BTreeSet<&str> = fields.keys().copied().collect();
    let read_names: BTreeSet<&str> = reported.keys().copied().collect();
    if written != read_names {
        return Err(format!(
            "the reader reports {:?} beyond what was written and misses {:?}",
            read_names.difference(&written).collect::<Vec<_>>(),
            written.difference(&read_names).collect::<Vec<_>>()
        ));
    }
    let mut expected_fields: BTreeSet<(String, String)> = BTreeSet::new();
    let written_corrections = corrections(xmp)?;
    for (name, field) in fields {
        let (list, text) = reported[name];
        match field.text() {
            Some(written) if written != text => {
                return Err(format!(
                    "{name} reads back as {text:?}, written {written:?}"
                ));
            }
            Some(_) => {}
            None => {
                let Field::Correction(correction) = field else {
                    unreachable!("only a correction has no text");
                };
                if list != "unsupported"
                    || !text.starts_with("What=Correction")
                    || !written_corrections.contains(correction)
                {
                    return Err(format!("{name} does not read back as written"));
                }
            }
        }
        // A calibrated value lands where the frozen calibration puts it, or is refused at its
        // shortfall (checked above with the other expected refusals).
        let calibrated = match field {
            Field::Number { value, .. } => luxforge_core::lightroom_calibration(name, *value),
            _ => None,
        };
        if let Some(Err(_)) = &calibrated {
            if list != "refused" {
                return Err(format!(
                    "{name} is {list}, not refused at its calibration's shortfall"
                ));
            }
            continue;
        }
        match counterpart(name) {
            Some(_) if list == "neutral" && neutral_grain(name, fields) => {}
            Some((action, target, How::Transfer)) => {
                if list != "mapped" {
                    return Err(format!("{name} is {list}, not transferred"));
                }
                let applied = read
                    .settings
                    .get(action)
                    .and_then(|fields| fields.get(target))
                    .ok_or(format!("{name} is not on {action}.{target}"))?;
                let wanted = match &calibrated {
                    Some(Ok(converted)) => json!(converted),
                    _ => expected(field).ok_or(format!("{name} has no value"))?,
                };
                if !same_value(applied, &wanted) {
                    return Err(format!(
                        "{name} lands on {action}.{target} as {applied}, written {wanted}"
                    ));
                }
                expected_fields.insert((action.to_owned(), target.to_owned()));
            }
            Some((_, _, How::Converted)) => {
                // White balance lands where the importer's conversion puts it: As Shot on the RAW
                // development's as-shot white with Basic's relative pair at 0, a custom pair on
                // the development's temperature and tint.
                for mapped in report.mapped.iter().filter(|m| m.setting == *name) {
                    let field = mapped.field.clone().unwrap_or_default();
                    let allowed = matches!(
                        (mapped.action.as_str(), field.as_str()),
                        ("set-raw", "white-balance" | "temperature" | "tint")
                            | (BASIC, "temperature" | "tint")
                    );
                    if !allowed {
                        return Err(format!("{name} converts onto {}.{field}", mapped.action));
                    }
                    expected_fields.insert((mapped.action.clone(), field));
                }
            }
            _ => {
                if list == "unsupported" && !UNMAPPED.contains(name) {
                    return Err(format!(
                        "{name} is reported unsupported, which the round does not expect"
                    ));
                }
            }
        }
    }
    for (name, entry) in &reported {
        if entry.0 == "unsupported" && !UNMAPPED.contains(name) {
            return Err(format!("{name} is reported unsupported"));
        }
    }
    let mut landed: BTreeSet<(String, String)> = BTreeSet::new();
    for (action, values) in &read.settings {
        for field in values.as_object().into_iter().flat_map(Map::keys) {
            landed.insert((action.clone(), field.clone()));
        }
    }
    if landed != expected_fields {
        return Err(format!(
            "the reader's settings hold {:?} beyond the written settings and miss {:?}",
            landed.difference(&expected_fields).collect::<Vec<_>>(),
            expected_fields.difference(&landed).collect::<Vec<_>>()
        ));
    }
    Ok(Readback {
        settings: read.settings,
        unsupported: report
            .unsupported
            .iter()
            .map(|entry| entry.setting.clone())
            .collect(),
        refused: report
            .refused
            .iter()
            .map(|entry| json!({"setting": entry.setting, "reason": entry.reason}))
            .collect(),
    })
}

/// The settings of `read` that differ from `baseline`'s, by action and field.
pub fn changes(read: &Map<String, Value>, baseline: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (action, fields) in read {
        let Some(fields) = fields.as_object() else {
            continue;
        };
        let mut changed = Map::new();
        for (field, value) in fields {
            if baseline.get(action).and_then(|b| b.get(field)) != Some(value) {
                changed.insert(field.clone(), value.clone());
            }
        }
        if !changed.is_empty() {
            out.insert(action.clone(), Value::Object(changed));
        }
    }
    out
}
