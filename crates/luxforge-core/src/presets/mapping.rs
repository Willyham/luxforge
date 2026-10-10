//! The one table of Lightroom settings the importer recognizes, and the rules that turn a file's
//! settings into a Luxforge settings set and its report.
//!
//! A mapped value is a **value transfer**: the same number on a Luxforge control with the same
//! name, range and direction, checked against that control's own descriptor. It is not a claim
//! that Luxforge renders what Lightroom renders; `docs/research/lightroom/slider-parity.md` records
//! why equal values do not mean equal pixels. A curve transfer carries Lightroom's composite point
//! curve onto the Tone curve's points, rescaled from Lightroom's 0–255 to 0–1.
//! A value that does not parse or lies outside the control's hard range is refused, never clamped.
use super::report::{DerivedSettings, ImportReport, MappedSetting, ReportedSetting};
use super::value::{
    RawSetting, RawValue, boolean, identity_curve, json_number, number, report_text,
};
use crate::Error;
use crate::{ModuleRegistry, ParameterKind, check_value};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

/// A Lightroom panel switch, `Enable<Panel>`, written by older templates. `false` means the
/// panel's values are not in effect in the preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Panel {
    ColorAdjustments,
    Effects,
    Detail,
    SplitToning,
    GrayscaleMix,
    Calibration,
    LensCorrections,
    Transform,
    ToneCurve,
    Retouch,
    RedEye,
    GradientBasedCorrections,
    CircularGradientBasedCorrections,
    PaintBasedCorrections,
    MaskGroupBasedCorrections,
}

pub(super) const PANELS: [Panel; 15] = [
    Panel::ColorAdjustments,
    Panel::Effects,
    Panel::Detail,
    Panel::SplitToning,
    Panel::GrayscaleMix,
    Panel::Calibration,
    Panel::LensCorrections,
    Panel::Transform,
    Panel::ToneCurve,
    Panel::Retouch,
    Panel::RedEye,
    Panel::GradientBasedCorrections,
    Panel::CircularGradientBasedCorrections,
    Panel::PaintBasedCorrections,
    Panel::MaskGroupBasedCorrections,
];

impl Panel {
    pub(super) fn switch(self) -> &'static str {
        match self {
            Self::ColorAdjustments => "EnableColorAdjustments",
            Self::Effects => "EnableEffects",
            Self::Detail => "EnableDetail",
            Self::SplitToning => "EnableSplitToning",
            Self::GrayscaleMix => "EnableGrayscaleMix",
            Self::Calibration => "EnableCalibration",
            Self::LensCorrections => "EnableLensCorrections",
            Self::Transform => "EnableTransform",
            Self::ToneCurve => "EnableToneCurve",
            Self::Retouch => "EnableRetouch",
            Self::RedEye => "EnableRedEye",
            Self::GradientBasedCorrections => "EnableGradientBasedCorrections",
            Self::CircularGradientBasedCorrections => "EnableCircularGradientBasedCorrections",
            Self::PaintBasedCorrections => "EnablePaintBasedCorrections",
            Self::MaskGroupBasedCorrections => "EnableMaskGroupBasedCorrections",
        }
    }
}

/// When an unsupported setting is at a value that changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Neutral {
    Never,
    Zero,
    Equals(f64),
    /// A boolean `false`, in any form Lightroom writes one.
    Off,
    /// A curve whose every point has `x == y`.
    Identity,
    /// Blank text, an empty sequence or an empty structure: no masks, spots or red-eye fixes.
    Empty,
    Text(&'static str),
}

/// When another setting of the same preset makes this one neutral, because this one only
/// qualifies an amount that is itself neutral. The other setting must be in the preset: one it
/// leaves out keeps the photo's own value, which the import cannot know.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Unless {
    Nothing,
    /// The named amount is 0.
    Zero(&'static str),
    /// The named switch is `false`.
    Off(&'static str),
    /// The named setting is in the preset, whatever its value.
    Present(&'static str),
    /// Every named amount the preset holds is 0, and it holds at least one of them.
    AllZero(&'static [&'static str]),
    /// The named profile is Lightroom's default.
    DefaultProfile(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Rule {
    /// A value transfer onto a presettable Luxforge field. `lightroom` is the range Lightroom
    /// writes, which the target's hard range must cover.
    Transfer {
        action: &'static str,
        field: &'static str,
        lightroom: (f64, f64),
    },
    /// A point curve transferred onto a presettable curve field: Lightroom's `"x, y"` points on
    /// its 0–255 scale, each coordinate divided by 255, in order ([`curve_transfer_value`]).
    CurveTransfer {
        action: &'static str,
        field: &'static str,
    },
    /// Lightroom's absolute RAW `Temperature` (K) or `Tint`, converted together through the
    /// illuminant chromaticity the pair names onto the RAW development's `field`
    /// ([`crate::lightroom_to_luxforge`]), and refused together when that white is out of range.
    RawWhiteBalance {
        field: &'static str,
    },
    /// `WhiteBalance`: `As Shot` is the RAW development's as-shot white balance, and Basic's
    /// relative pair at 0 when the preset holds no incremental value; `Custom` is neutral when the
    /// values it names are mapped and refused otherwise; `Auto` and named modes are refused.
    WhiteBalance,
    AutoTone,
    /// `CameraProfile` or a nested `Look`: neutral for Lightroom's default profile.
    Profile,
    /// A field of a process version before 2012: neutral in a modern preset, refused in a legacy
    /// one.
    EarlierProcess,
    Unsupported {
        reason: &'static str,
        neutral: Neutral,
        unless: Unless,
    },
    /// Preset metadata: recorded in the origin where useful, never reported.
    Metadata,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Row {
    pub name: &'static str,
    pub rule: Rule,
    pub panel: Option<Panel>,
}

const fn transfer(
    name: &'static str,
    action: &'static str,
    field: &'static str,
    lightroom: (f64, f64),
    panel: Option<Panel>,
) -> Row {
    Row {
        name,
        rule: Rule::Transfer {
            action,
            field,
            lightroom,
        },
        panel,
    }
}

const fn row(name: &'static str, rule: Rule, panel: Option<Panel>) -> Row {
    Row { name, rule, panel }
}

const fn unsupported_row(
    name: &'static str,
    reason: &'static str,
    neutral: Neutral,
    unless: Unless,
    panel: Option<Panel>,
) -> Row {
    Row {
        name,
        rule: Rule::Unsupported {
            reason,
            neutral,
            unless,
        },
        panel,
    }
}

const fn metadata(name: &'static str) -> Row {
    row(name, Rule::Metadata, None)
}

const SIGNED: (f64, f64) = (-100.0, 100.0);
const UNSIGNED: (f64, f64) = (0.0, 100.0);
/// A hue in degrees, as Lightroom's split-toning and colour-grading hues are written.
const HUE: (f64, f64) = (0.0, 360.0);
const BASIC: &str = "set-basic";
/// The RAW development's white-balance patch, which Lightroom's absolute white balance maps onto.
const RAW: &str = "set-raw";
const PRESENCE: &str = "set-presence";
const MIXER: &str = "set-mixer";
const VIGNETTE: &str = "set-vignette";
const TONE_CURVE: &str = "set-curve";
const HSL: Option<Panel> = Some(Panel::ColorAdjustments);
const EFFECTS: Option<Panel> = Some(Panel::Effects);
const DETAIL: Option<Panel> = Some(Panel::Detail);
const GRADING: Option<Panel> = Some(Panel::SplitToning);
const LENS: Option<Panel> = Some(Panel::LensCorrections);
const TRANSFORM: Option<Panel> = Some(Panel::Transform);
const CURVE: Option<Panel> = Some(Panel::ToneCurve);
const CALIBRATION: Option<Panel> = Some(Panel::Calibration);

const NO_SHARPENING: &str = "Lightroom sharpening is not mapped to Luxforge Detail";
const NO_NOISE_REDUCTION: &str = "Lightroom noise reduction is not mapped to Luxforge Detail";
const NO_GRAIN: &str = "Luxforge has no grain";
const NO_CHANNEL_CURVES: &str = "Luxforge's tone curve has no per-channel curves";
const NO_PARAMETRIC: &str = "Luxforge has no parametric curve";
const NO_CURVE_SATURATION: &str = "Luxforge's tone curve changes no saturation";
const CURVE_NAME: &str = "names a curve whose points are not in the preset";
const NO_GRAYSCALE: &str = "Luxforge has no black-and-white conversion";
const NO_LENS: &str = "Luxforge has no lens corrections";
const NO_CA: &str = "Luxforge has no chromatic-aberration correction";
const NO_LENS_VIGNETTE: &str = "Luxforge has no lens vignetting correction";
const NO_DEFRINGE: &str = "Luxforge has no defringe";
const NO_TRANSFORM: &str = "different perspective model";
const NO_CALIBRATION: &str = "Luxforge has no calibration";
const NO_PROFILES: &str = "Luxforge has no profiles";
const NO_MASKS: &str = "Lightroom masks and local corrections are not imported";
const NO_SPOTS: &str = "Luxforge has no spot removal";
const NO_RED_EYE: &str = "Luxforge has no red-eye correction";
const NO_MOIRE: &str = "Luxforge has no moiré reduction";
const CROP: &str = "crop belongs to one photo, not to a preset";
const VIGNETTE_STYLE: &str = "Luxforge draws one vignette style";
const VIGNETTE_HIGHLIGHTS: &str = "Luxforge's vignette has no highlight contrast";
pub(super) const NOT_RECOGNISED: &str = "not recognised";
pub(super) const DISABLED: &str = "disabled in the preset";

const PARAMETRIC_REGIONS: &[&str] = &[
    "ParametricShadows",
    "ParametricDarks",
    "ParametricLights",
    "ParametricHighlights",
];
/// The settings Lightroom added with Color Grading (Camera Raw 13.0), which an earlier document
/// cannot hold.
const COLOR_GRADE: &[&str] = &[
    "ColorGradeMidtoneHue",
    "ColorGradeMidtoneSat",
    "ColorGradeGlobalHue",
    "ColorGradeGlobalSat",
    "ColorGradeShadowLum",
    "ColorGradeMidtoneLum",
    "ColorGradeHighlightLum",
    "ColorGradeGlobalLum",
    "ColorGradeBlending",
];

/// The Camera Raw version that introduced Color Grading. A document written by an earlier version
/// holds only split toning.
const COLOR_GRADING_VERSION: (u32, u32) = (13, 0);

/// Why a split-toning document carries [`legacy_split_toning`].
const SPLIT_TONING: &str = "split toning written before Color Grading: Blending 100 and every \
                            control Color Grading added at 0";
/// Why [`legacy_split_toning`] is left out.
const SPLIT_TONING_REFUSED: &str =
    "a split-toning setting is refused, so the document's split toning is not reproduced";

/// What a legacy split-toning document means for the controls Color Grading added: Blending 100,
/// split toning's full overlap, and every other one at its neutral 0, on the fields [`ROWS`]
/// transfers those settings to. Written beside the transferred split-toning values, never over
/// them, since [`COLOR_GRADE`] holds none of split toning's own settings.
fn legacy_split_toning() -> impl Iterator<Item = (&'static str, &'static str, f64)> {
    COLOR_GRADE
        .iter()
        .filter_map(|name| match lookup(name)?.rule {
            Rule::Transfer { action, field, .. } => {
                let applied = if *name == "ColorGradeBlending" {
                    100.0
                } else {
                    0.0
                };
                Some((action, field, applied))
            }
            _ => None,
        })
}

use Neutral::{Empty, Equals, Identity, Never, Off, Zero};

/// Every recognized setting, exactly once. [`PREFIXES`] covers families named by a prefix.
pub(super) const ROWS: &[Row] = &[
    // Value transfers.
    transfer("Exposure2012", BASIC, "exposure", (-5.0, 5.0), None),
    transfer("Contrast2012", BASIC, "contrast", SIGNED, None),
    transfer("Highlights2012", BASIC, "highlights", SIGNED, None),
    transfer("Shadows2012", BASIC, "shadows", SIGNED, None),
    transfer("Whites2012", BASIC, "whites", SIGNED, None),
    transfer("Blacks2012", BASIC, "blacks", SIGNED, None),
    transfer("Vibrance", BASIC, "vibrance", SIGNED, None),
    transfer("Saturation", BASIC, "saturation", SIGNED, None),
    transfer("IncrementalTemperature", BASIC, "temperature", SIGNED, None),
    transfer("IncrementalTint", BASIC, "tint", SIGNED, None),
    transfer("Texture", PRESENCE, "texture", SIGNED, None),
    transfer("Clarity2012", PRESENCE, "clarity", SIGNED, None),
    transfer("Dehaze", PRESENCE, "dehaze", SIGNED, None),
    transfer("HueAdjustmentRed", MIXER, "red-hue", SIGNED, HSL),
    transfer("HueAdjustmentOrange", MIXER, "orange-hue", SIGNED, HSL),
    transfer("HueAdjustmentYellow", MIXER, "yellow-hue", SIGNED, HSL),
    transfer("HueAdjustmentGreen", MIXER, "green-hue", SIGNED, HSL),
    transfer("HueAdjustmentAqua", MIXER, "aqua-hue", SIGNED, HSL),
    transfer("HueAdjustmentBlue", MIXER, "blue-hue", SIGNED, HSL),
    transfer("HueAdjustmentPurple", MIXER, "purple-hue", SIGNED, HSL),
    transfer("HueAdjustmentMagenta", MIXER, "magenta-hue", SIGNED, HSL),
    transfer(
        "SaturationAdjustmentRed",
        MIXER,
        "red-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentOrange",
        MIXER,
        "orange-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentYellow",
        MIXER,
        "yellow-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentGreen",
        MIXER,
        "green-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentAqua",
        MIXER,
        "aqua-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentBlue",
        MIXER,
        "blue-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentPurple",
        MIXER,
        "purple-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "SaturationAdjustmentMagenta",
        MIXER,
        "magenta-saturation",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentRed",
        MIXER,
        "red-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentOrange",
        MIXER,
        "orange-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentYellow",
        MIXER,
        "yellow-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentGreen",
        MIXER,
        "green-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentAqua",
        MIXER,
        "aqua-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentBlue",
        MIXER,
        "blue-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentPurple",
        MIXER,
        "purple-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "LuminanceAdjustmentMagenta",
        MIXER,
        "magenta-luminance",
        SIGNED,
        HSL,
    ),
    transfer(
        "PostCropVignetteAmount",
        VIGNETTE,
        "amount",
        SIGNED,
        EFFECTS,
    ),
    transfer(
        "PostCropVignetteMidpoint",
        VIGNETTE,
        "midpoint",
        UNSIGNED,
        EFFECTS,
    ),
    transfer(
        "PostCropVignetteRoundness",
        VIGNETTE,
        "roundness",
        SIGNED,
        EFFECTS,
    ),
    transfer(
        "PostCropVignetteFeather",
        VIGNETTE,
        "feather",
        UNSIGNED,
        EFFECTS,
    ),
    // White balance and profiles.
    row(
        "Temperature",
        Rule::RawWhiteBalance {
            field: "temperature",
        },
        None,
    ),
    row("Tint", Rule::RawWhiteBalance { field: "tint" }, None),
    row("WhiteBalance", Rule::WhiteBalance, None),
    row("CameraProfile", Rule::Profile, None),
    row("Look", Rule::Profile, None),
    unsupported_row(
        "CameraProfileDigest",
        NO_PROFILES,
        Never,
        Unless::DefaultProfile("CameraProfile"),
        None,
    ),
    // Fields of process versions before 2012.
    row("Exposure", Rule::EarlierProcess, None),
    row("Contrast", Rule::EarlierProcess, None),
    row("Brightness", Rule::EarlierProcess, None),
    row("Shadows", Rule::EarlierProcess, None),
    row("FillLight", Rule::EarlierProcess, None),
    row("HighlightRecovery", Rule::EarlierProcess, None),
    row("Clarity", Rule::EarlierProcess, None),
    row("ToneCurve", Rule::EarlierProcess, CURVE),
    row("ToneCurveName", Rule::EarlierProcess, CURVE),
    row("AutoExposure", Rule::EarlierProcess, None),
    row("AutoShadows", Rule::EarlierProcess, None),
    row("AutoBrightness", Rule::EarlierProcess, None),
    row("AutoContrast", Rule::EarlierProcess, None),
    // Tone curves: the composite point curve transfers onto the luminance curve; Luxforge has no
    // per-channel or parametric curve.
    row(
        "ToneCurvePV2012",
        Rule::CurveTransfer {
            action: TONE_CURVE,
            field: "luminance",
        },
        CURVE,
    ),
    unsupported_row(
        "ToneCurvePV2012Red",
        NO_CHANNEL_CURVES,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ToneCurvePV2012Green",
        NO_CHANNEL_CURVES,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ToneCurvePV2012Blue",
        NO_CHANNEL_CURVES,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    // The name of the points `ToneCurvePV2012` carries, which say everything the name does.
    unsupported_row(
        "ToneCurveName2012",
        CURVE_NAME,
        Neutral::Text("Linear"),
        Unless::Present("ToneCurvePV2012"),
        CURVE,
    ),
    unsupported_row(
        "ParametricShadows",
        NO_PARAMETRIC,
        Zero,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ParametricDarks",
        NO_PARAMETRIC,
        Zero,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ParametricLights",
        NO_PARAMETRIC,
        Zero,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ParametricHighlights",
        NO_PARAMETRIC,
        Zero,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ParametricShadowSplit",
        NO_PARAMETRIC,
        Equals(25.0),
        Unless::AllZero(PARAMETRIC_REGIONS),
        CURVE,
    ),
    unsupported_row(
        "ParametricMidtoneSplit",
        NO_PARAMETRIC,
        Equals(50.0),
        Unless::AllZero(PARAMETRIC_REGIONS),
        CURVE,
    ),
    unsupported_row(
        "ParametricHighlightSplit",
        NO_PARAMETRIC,
        Equals(75.0),
        Unless::AllZero(PARAMETRIC_REGIONS),
        CURVE,
    ),
    unsupported_row(
        "CurveRefineSaturation",
        NO_CURVE_SATURATION,
        Equals(100.0),
        Unless::Nothing,
        CURVE,
    ),
    // Split toning and colour grading, transferred onto the mixer's grading fields. Which
    // Lightroom grading model a document was written for is decided once per document
    // ([`grade_era`]), and a legacy split-toning document gains the later fields' values
    // ([`LEGACY_SPLIT_TONING`]).
    transfer(
        "SplitToningShadowHue",
        MIXER,
        "grade-shadows-hue",
        HUE,
        GRADING,
    ),
    transfer(
        "SplitToningShadowSaturation",
        MIXER,
        "grade-shadows-saturation",
        UNSIGNED,
        GRADING,
    ),
    transfer(
        "SplitToningHighlightHue",
        MIXER,
        "grade-highlights-hue",
        HUE,
        GRADING,
    ),
    transfer(
        "SplitToningHighlightSaturation",
        MIXER,
        "grade-highlights-saturation",
        UNSIGNED,
        GRADING,
    ),
    transfer(
        "SplitToningBalance",
        MIXER,
        "grade-balance",
        SIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeMidtoneHue",
        MIXER,
        "grade-midtones-hue",
        HUE,
        GRADING,
    ),
    transfer(
        "ColorGradeMidtoneSat",
        MIXER,
        "grade-midtones-saturation",
        UNSIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeGlobalHue",
        MIXER,
        "grade-global-hue",
        HUE,
        GRADING,
    ),
    transfer(
        "ColorGradeGlobalSat",
        MIXER,
        "grade-global-saturation",
        UNSIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeShadowLum",
        MIXER,
        "grade-shadows-luminance",
        SIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeMidtoneLum",
        MIXER,
        "grade-midtones-luminance",
        SIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeHighlightLum",
        MIXER,
        "grade-highlights-luminance",
        SIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeGlobalLum",
        MIXER,
        "grade-global-luminance",
        SIGNED,
        GRADING,
    ),
    transfer(
        "ColorGradeBlending",
        MIXER,
        "grade-blending",
        UNSIGNED,
        GRADING,
    ),
    // Detail: sharpening and noise reduction.
    unsupported_row("Sharpness", NO_SHARPENING, Zero, Unless::Nothing, DETAIL),
    unsupported_row(
        "SharpenRadius",
        NO_SHARPENING,
        Never,
        Unless::Zero("Sharpness"),
        DETAIL,
    ),
    unsupported_row(
        "SharpenDetail",
        NO_SHARPENING,
        Never,
        Unless::Zero("Sharpness"),
        DETAIL,
    ),
    unsupported_row(
        "SharpenEdgeMasking",
        NO_SHARPENING,
        Never,
        Unless::Zero("Sharpness"),
        DETAIL,
    ),
    unsupported_row(
        "LuminanceSmoothing",
        NO_NOISE_REDUCTION,
        Zero,
        Unless::Nothing,
        DETAIL,
    ),
    unsupported_row(
        "LuminanceNoiseReductionDetail",
        NO_NOISE_REDUCTION,
        Never,
        Unless::Zero("LuminanceSmoothing"),
        DETAIL,
    ),
    unsupported_row(
        "LuminanceNoiseReductionContrast",
        NO_NOISE_REDUCTION,
        Never,
        Unless::Zero("LuminanceSmoothing"),
        DETAIL,
    ),
    unsupported_row(
        "ColorNoiseReduction",
        NO_NOISE_REDUCTION,
        Zero,
        Unless::Nothing,
        DETAIL,
    ),
    unsupported_row(
        "ColorNoiseReductionDetail",
        NO_NOISE_REDUCTION,
        Never,
        Unless::Zero("ColorNoiseReduction"),
        DETAIL,
    ),
    unsupported_row(
        "ColorNoiseReductionSmoothness",
        NO_NOISE_REDUCTION,
        Never,
        Unless::Zero("ColorNoiseReduction"),
        DETAIL,
    ),
    // Effects: grain and the post-crop vignette's other controls.
    unsupported_row("GrainAmount", NO_GRAIN, Zero, Unless::Nothing, EFFECTS),
    unsupported_row(
        "GrainSize",
        NO_GRAIN,
        Never,
        Unless::Zero("GrainAmount"),
        EFFECTS,
    ),
    unsupported_row(
        "GrainFrequency",
        NO_GRAIN,
        Never,
        Unless::Zero("GrainAmount"),
        EFFECTS,
    ),
    unsupported_row(
        "GrainSeed",
        NO_GRAIN,
        Never,
        Unless::Zero("GrainAmount"),
        EFFECTS,
    ),
    unsupported_row(
        "PostCropVignetteStyle",
        VIGNETTE_STYLE,
        Equals(1.0),
        Unless::Zero("PostCropVignetteAmount"),
        EFFECTS,
    ),
    unsupported_row(
        "PostCropVignetteHighlightContrast",
        VIGNETTE_HIGHLIGHTS,
        Zero,
        Unless::Zero("PostCropVignetteAmount"),
        EFFECTS,
    ),
    // Black and white.
    unsupported_row(
        "ConvertToGrayscale",
        NO_GRAYSCALE,
        Off,
        Unless::Nothing,
        None,
    ),
    unsupported_row(
        "AutoGrayscaleMix",
        NO_GRAYSCALE,
        Off,
        Unless::Off("ConvertToGrayscale"),
        Some(Panel::GrayscaleMix),
    ),
    gray_mix("GrayMixerRed"),
    gray_mix("GrayMixerOrange"),
    gray_mix("GrayMixerYellow"),
    gray_mix("GrayMixerGreen"),
    gray_mix("GrayMixerAqua"),
    gray_mix("GrayMixerBlue"),
    gray_mix("GrayMixerPurple"),
    gray_mix("GrayMixerMagenta"),
    // Lens corrections.
    unsupported_row("LensProfileEnable", NO_LENS, Off, Unless::Nothing, LENS),
    unsupported_row(
        "LensManualDistortionAmount",
        NO_LENS,
        Zero,
        Unless::Nothing,
        LENS,
    ),
    unsupported_row("AutoLateralCA", NO_CA, Off, Unless::Nothing, LENS),
    unsupported_row("ChromaticAberrationR", NO_CA, Zero, Unless::Nothing, LENS),
    unsupported_row("ChromaticAberrationB", NO_CA, Zero, Unless::Nothing, LENS),
    unsupported_row(
        "VignetteAmount",
        NO_LENS_VIGNETTE,
        Zero,
        Unless::Nothing,
        LENS,
    ),
    unsupported_row(
        "VignetteMidpoint",
        NO_LENS_VIGNETTE,
        Never,
        Unless::Zero("VignetteAmount"),
        LENS,
    ),
    unsupported_row("Defringe", NO_DEFRINGE, Zero, Unless::Nothing, LENS),
    unsupported_row(
        "DefringePurpleAmount",
        NO_DEFRINGE,
        Zero,
        Unless::Nothing,
        LENS,
    ),
    unsupported_row(
        "DefringePurpleHueLo",
        NO_DEFRINGE,
        Never,
        Unless::Zero("DefringePurpleAmount"),
        LENS,
    ),
    unsupported_row(
        "DefringePurpleHueHi",
        NO_DEFRINGE,
        Never,
        Unless::Zero("DefringePurpleAmount"),
        LENS,
    ),
    unsupported_row(
        "DefringeGreenAmount",
        NO_DEFRINGE,
        Zero,
        Unless::Nothing,
        LENS,
    ),
    unsupported_row(
        "DefringeGreenHueLo",
        NO_DEFRINGE,
        Never,
        Unless::Zero("DefringeGreenAmount"),
        LENS,
    ),
    unsupported_row(
        "DefringeGreenHueHi",
        NO_DEFRINGE,
        Never,
        Unless::Zero("DefringeGreenAmount"),
        LENS,
    ),
    // Transform and upright.
    unsupported_row(
        "PerspectiveUpright",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveVertical",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveHorizontal",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveRotate",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveAspect",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveX",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveY",
        NO_TRANSFORM,
        Zero,
        Unless::Nothing,
        TRANSFORM,
    ),
    unsupported_row(
        "PerspectiveScale",
        NO_TRANSFORM,
        Equals(100.0),
        Unless::Nothing,
        TRANSFORM,
    ),
    // Calibration.
    unsupported_row(
        "ShadowTint",
        NO_CALIBRATION,
        Zero,
        Unless::Nothing,
        CALIBRATION,
    ),
    unsupported_row("RedHue", NO_CALIBRATION, Zero, Unless::Nothing, CALIBRATION),
    unsupported_row(
        "RedSaturation",
        NO_CALIBRATION,
        Zero,
        Unless::Nothing,
        CALIBRATION,
    ),
    unsupported_row(
        "GreenHue",
        NO_CALIBRATION,
        Zero,
        Unless::Nothing,
        CALIBRATION,
    ),
    unsupported_row(
        "GreenSaturation",
        NO_CALIBRATION,
        Zero,
        Unless::Nothing,
        CALIBRATION,
    ),
    unsupported_row(
        "BlueHue",
        NO_CALIBRATION,
        Zero,
        Unless::Nothing,
        CALIBRATION,
    ),
    unsupported_row(
        "BlueSaturation",
        NO_CALIBRATION,
        Zero,
        Unless::Nothing,
        CALIBRATION,
    ),
    // Auto Tone, masks, retouching and moiré.
    row("AutoTone", Rule::AutoTone, None),
    unsupported_row(
        "MaskGroupBasedCorrections",
        NO_MASKS,
        Empty,
        Unless::Nothing,
        Some(Panel::MaskGroupBasedCorrections),
    ),
    unsupported_row(
        "GradientBasedCorrections",
        NO_MASKS,
        Empty,
        Unless::Nothing,
        Some(Panel::GradientBasedCorrections),
    ),
    unsupported_row(
        "CircularGradientBasedCorrections",
        NO_MASKS,
        Empty,
        Unless::Nothing,
        Some(Panel::CircularGradientBasedCorrections),
    ),
    unsupported_row(
        "PaintBasedCorrections",
        NO_MASKS,
        Empty,
        Unless::Nothing,
        Some(Panel::PaintBasedCorrections),
    ),
    unsupported_row(
        "RetouchInfo",
        NO_SPOTS,
        Empty,
        Unless::Nothing,
        Some(Panel::Retouch),
    ),
    unsupported_row(
        "RetouchAreas",
        NO_SPOTS,
        Empty,
        Unless::Nothing,
        Some(Panel::Retouch),
    ),
    unsupported_row(
        "RedEyeInfo",
        NO_RED_EYE,
        Empty,
        Unless::Nothing,
        Some(Panel::RedEye),
    ),
    unsupported_row("Moire", NO_MOIRE, Zero, Unless::Nothing, None),
    // Crop, which only a photo's sidecar holds.
    unsupported_row("HasCrop", CROP, Off, Unless::Nothing, None),
    unsupported_row("CropTop", CROP, Zero, Unless::Off("HasCrop"), None),
    unsupported_row("CropLeft", CROP, Zero, Unless::Off("HasCrop"), None),
    unsupported_row(
        "CropBottom",
        CROP,
        Equals(1.0),
        Unless::Off("HasCrop"),
        None,
    ),
    unsupported_row("CropRight", CROP, Equals(1.0), Unless::Off("HasCrop"), None),
    unsupported_row("CropAngle", CROP, Zero, Unless::Off("HasCrop"), None),
    unsupported_row(
        "CropConstrainToWarp",
        CROP,
        Zero,
        Unless::Off("HasCrop"),
        None,
    ),
    unsupported_row("CropUnit", CROP, Never, Unless::Off("HasCrop"), None),
    unsupported_row("CropWidth", CROP, Never, Unless::Off("HasCrop"), None),
    unsupported_row("CropHeight", CROP, Never, Unless::Off("HasCrop"), None),
    // Preset metadata and sidecar bookkeeping.
    metadata("PresetType"),
    metadata("UUID"),
    metadata("Cluster"),
    metadata("Version"),
    metadata("ProcessVersion"),
    metadata("HasSettings"),
    metadata("RequiresRGBTables"),
    metadata("CameraModelRestriction"),
    metadata("Copyright"),
    metadata("ContactInfo"),
    metadata("Name"),
    metadata("ShortName"),
    metadata("SortName"),
    metadata("Group"),
    metadata("Description"),
    metadata("RawFileName"),
    metadata("AlreadyApplied"),
];

const fn gray_mix(name: &'static str) -> Row {
    unsupported_row(
        name,
        NO_GRAYSCALE,
        Never,
        Unless::Off("ConvertToGrayscale"),
        Some(Panel::GrayscaleMix),
    )
}

/// Families named by a prefix, consulted after [`ROWS`] finds no exact name.
pub(super) const PREFIXES: &[Row] = &[
    // `SupportsAmount`, `SupportsColor`, `SupportsMonochrome` and the dynamic-range flags.
    metadata("Supports"),
    // Every lens profile field qualifies `LensProfileEnable`.
    unsupported_row(
        "LensProfile",
        NO_LENS,
        Never,
        Unless::Off("LensProfileEnable"),
        LENS,
    ),
    // Upright's version, centre, focal and guide bookkeeping qualifies `PerspectiveUpright`.
    unsupported_row(
        "Upright",
        NO_TRANSFORM,
        Never,
        Unless::Zero("PerspectiveUpright"),
        TRANSFORM,
    ),
];

/// Lightroom's default profiles, which map to neutral because Luxforge's looks are its own.
const DEFAULT_PROFILES: [&str; 2] = ["Adobe Standard", "Adobe Color"];

/// Whether a setting holds a tone curve, so a template's flat array is read as its points.
pub(crate) fn is_curve(name: &str) -> bool {
    name == "ToneCurve" || name.starts_with("ToneCurvePV2012")
}

fn exact() -> &'static HashMap<&'static str, &'static Row> {
    static EXACT: OnceLock<HashMap<&'static str, &'static Row>> = OnceLock::new();
    EXACT.get_or_init(|| ROWS.iter().map(|row| (row.name, row)).collect())
}

pub(super) fn lookup(name: &str) -> Option<&'static Row> {
    exact().get(name).copied().or_else(|| {
        PREFIXES
            .iter()
            .find(|row| name.starts_with(row.name) && name.len() > row.name.len())
    })
}

/// A preset's process version. Luxforge's controls follow the Process 2012 names, so only a
/// modern preset transfers values.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Era {
    Modern,
    /// Every mapped setting is refused with this reason.
    Legacy(String),
}

/// The 2012 tone fields whose presence makes a preset without `ProcessVersion` modern.
const MODERN_TONE: &[&str] = &[
    "Exposure2012",
    "Contrast2012",
    "Highlights2012",
    "Shadows2012",
    "Whites2012",
    "Blacks2012",
    "Clarity2012",
    "ToneCurvePV2012",
];
/// The earlier-process tone fields.
const LEGACY_TONE: &[&str] = &[
    "Exposure",
    "Contrast",
    "Brightness",
    "Shadows",
    "FillLight",
    "HighlightRecovery",
    "Clarity",
    "ToneCurve",
];

fn digits(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
}

/// `major[.minor]` as written, without a sign, exponent or anything after the minor digits: a
/// `ProcessVersion`.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.trim().split_once('.').unwrap_or((text.trim(), "0"));
    if !digits(major) || !digits(minor) {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// A Camera Raw `Version`, `major[.minor[.patch]]`: Camera Raw writes point releases such as
/// `11.4.1`, and only the major and minor decide which grading model wrote the document.
fn parse_camera_raw_version(text: &str) -> Option<(u32, u32)> {
    let text = text.trim();
    match text.rsplit_once('.') {
        Some((release, patch)) if release.contains('.') => {
            digits(patch).then(|| parse_version(release)).flatten()
        }
        _ => parse_version(text),
    }
}

fn era(settings: &HashMap<&str, &RawValue>) -> Era {
    match settings.get("ProcessVersion") {
        Some(value) => {
            let written = report_text(value);
            match value.text().and_then(parse_version) {
                Some(version) if version >= (6, 7) => Era::Modern,
                Some(_) => Era::Legacy(format!("earlier process version {written}")),
                None => Era::Legacy(format!("unrecognised process version {written}")),
            }
        }
        None => {
            let has = |names: &[&str]| names.iter().any(|name| settings.contains_key(name));
            if has(LEGACY_TONE) && !has(MODERN_TONE) {
                Era::Legacy("earlier process version".into())
            } else {
                Era::Modern
            }
        }
    }
}

/// Which Lightroom grading model a document's split-toning and colour-grading settings were
/// written for, decided from the document's own evidence rather than from a setting it leaves out.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GradeEra {
    /// Color Grading (Camera Raw 13.0 and later): each setting is a direct transfer of its own
    /// field, and a field the document leaves out keeps the photo's value.
    ColorGrading,
    /// Split toning only: the transferred values gain [`legacy_split_toning`].
    SplitToning,
    /// The document does not show which: every grading setting is refused with this reason.
    Ambiguous(String),
}

/// A `.lrtemplate` preset predates Color Grading, since Lightroom stopped writing templates before
/// Color Grading existed; an XMP document's Camera Raw `Version` says which model it was written
/// for. A Color Grading setting is itself evidence of the later model, except where the format or
/// the version says it cannot exist. A document with neither a version nor a Color Grading setting
/// is ambiguous, because split toning alone is written by both models.
fn grade_era(format: &str, settings: &HashMap<&str, &RawValue>) -> GradeEra {
    /// What the document says about the version that wrote it.
    enum Written {
        Template,
        Before,
        Since,
        Unreadable,
        Missing,
    }
    let version = settings.get("Version");
    let written = if format == super::FORMAT_TEMPLATE {
        Written::Template
    } else {
        match version.map(|value| value.text().and_then(parse_camera_raw_version)) {
            Some(Some(version)) if version >= COLOR_GRADING_VERSION => Written::Since,
            Some(Some(_)) => Written::Before,
            Some(None) => Written::Unreadable,
            None => Written::Missing,
        }
    };
    let text = || version.map(|value| report_text(value)).unwrap_or_default();
    let color_grade = COLOR_GRADE.iter().any(|name| settings.contains_key(name));
    match (written, color_grade) {
        (Written::Since, _) | (Written::Unreadable | Written::Missing, true) => {
            GradeEra::ColorGrading
        }
        (Written::Template | Written::Before, false) => GradeEra::SplitToning,
        (Written::Template, true) => GradeEra::Ambiguous(
            "Color Grading settings in a .lrtemplate preset, which Lightroom wrote only before \
             Color Grading existed"
                .into(),
        ),
        (Written::Before, true) => GradeEra::Ambiguous(format!(
            "Color Grading settings in a document written by Camera Raw {}, before Color Grading \
             existed",
            text()
        )),
        (Written::Unreadable, false) => GradeEra::Ambiguous(format!(
            "unrecognised Camera Raw version {}, so split toning cannot be told from Color \
             Grading",
            text()
        )),
        (Written::Missing, false) => GradeEra::Ambiguous(
            "no Camera Raw version, so split toning cannot be told from Color Grading".into(),
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Switch {
    On,
    Off,
    /// The switch is present but not a boolean, so the panel's state is unknown.
    Unknown,
}

fn is_zero(value: Option<&&RawValue>) -> bool {
    value.and_then(|value| number(value)) == Some(0.0)
}

fn is_default_profile(value: &RawValue) -> bool {
    let name = match value {
        RawValue::Struct(_) => value.field("Name").and_then(RawValue::text),
        other => other.text(),
    };
    name.is_some_and(|name| DEFAULT_PROFILES.contains(&name.trim()))
}

fn own_neutral(neutral: Neutral, value: &RawValue) -> bool {
    match neutral {
        Neutral::Never => false,
        Neutral::Zero => number(value) == Some(0.0),
        Neutral::Equals(expected) => number(value) == Some(expected),
        Neutral::Off => boolean(value) == Some(false),
        Neutral::Identity => identity_curve(value),
        Neutral::Empty => value.is_empty(),
        Neutral::Text(expected) => value.text().is_some_and(|text| text.trim() == expected),
    }
}

fn unless(unless: Unless, settings: &HashMap<&str, &RawValue>) -> bool {
    match unless {
        Unless::Nothing => false,
        Unless::Zero(name) => is_zero(settings.get(name)),
        Unless::Off(name) => settings
            .get(name)
            .is_some_and(|value| boolean(value) == Some(false)),
        Unless::Present(name) => settings.contains_key(name),
        Unless::AllZero(names) => {
            let present: Vec<_> = names.iter().filter_map(|name| settings.get(name)).collect();
            !present.is_empty() && present.into_iter().all(|value| is_zero(Some(value)))
        }
        Unless::DefaultProfile(name) => settings
            .get(name)
            .is_some_and(|value| is_default_profile(value)),
    }
}

/// The value a transfer writes, or why it cannot: the target must be presettable
/// ([`ModuleRegistry::patch_action`], whose refusal is the reason), and the value must pass the
/// target parameter's own check.
fn transfer_value(
    registry: &ModuleRegistry,
    action: &str,
    field: &str,
    value: &RawValue,
) -> Result<Value, String> {
    let (_, descriptor) = registry
        .patch_action(action)
        .map_err(|refusal| refusal.detail)?;
    let Some(parameter) = descriptor.parameter(field) else {
        return Err(format!("{action} has no {field} field"));
    };
    let applied = json_number(value).ok_or_else(|| "not a number".to_owned())?;
    check_value(parameter, &applied).map_err(|_| match &parameter.kind {
        ParameterKind::Number { min, max } => format!("outside Luxforge's range {min}..{max}"),
        ParameterKind::Integer { min, max } => {
            format!("not a whole number within Luxforge's range {min}..{max}")
        }
        _ => "not a value Luxforge accepts".to_owned(),
    })?;
    Ok(applied)
}

/// Lightroom's `ToneCurvePV2012` as the value a curve transfer writes, or why it cannot. The target
/// must be presettable and declare `field` as a curve. The value is then read in stages, each over
/// every point before the next begins, and the first failure is the reason: a list; a count within
/// the target's point bounds, before any item is read; every item `"x, y"`, split once at `,`,
/// each part trimmed and read as Lightroom writes a number; every coordinate within Lightroom's
/// `0..=255`; every input greater than the one before; no output less than the one before. A point
/// is named by its zero-based index, as the host names one. The value is `[x / 255, y / 255]` per
/// point, in order, never decimated, clamped or reordered, and it must pass the target parameter's
/// own check. At most `points_max` items are ever read, whatever the list holds.
fn curve_transfer_value(
    registry: &ModuleRegistry,
    action: &str,
    field: &str,
    value: &RawValue,
) -> Result<Value, String> {
    let (_, descriptor) = registry
        .patch_action(action)
        .map_err(|refusal| refusal.detail)?;
    let Some(parameter) = descriptor.parameter(field) else {
        return Err(format!("{action} has no {field} field"));
    };
    let ParameterKind::Curve {
        points_min,
        points_max,
        ..
    } = &parameter.kind
    else {
        return Err(format!("{action}.{field} is not a curve"));
    };
    let RawValue::List(items) = value else {
        return Err("not a list of \"x, y\" points".to_owned());
    };
    let count = items.len();
    if count > *points_max {
        return Err(format!(
            "has {count} points; Luxforge's tone curve holds at most {points_max}"
        ));
    }
    if count < *points_min {
        let points = if count == 1 { "point" } else { "points" };
        return Err(format!(
            "has {count} {points}; Luxforge's tone curve needs at least {points_min}"
        ));
    }
    let pair = |item: &RawValue| -> Option<[f64; 2]> {
        let (x, y) = item.text()?.split_once(',')?;
        let read = |part: &str| number(&RawValue::Text(part.trim().to_owned()));
        Some([read(x)?, read(y)?])
    };
    let points = items
        .iter()
        .enumerate()
        .map(|(index, item)| pair(item).ok_or_else(|| format!("point {index} is not \"x, y\"")))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(index) = points
        .iter()
        .position(|point| point.iter().any(|value| !(0.0..=255.0).contains(value)))
    {
        return Err(format!("point {index} is outside 0..255"));
    }
    if let Some(index) = (1..count).find(|&index| points[index][0] <= points[index - 1][0]) {
        return Err(format!("point {index}'s input does not increase"));
    }
    if let Some(index) = (1..count).find(|&index| points[index][1] < points[index - 1][1]) {
        return Err(format!(
            "point {index}'s output decreases; Luxforge's tone curve is monotone"
        ));
    }
    let applied = Value::Array(
        points
            .iter()
            .map(|[x, y]| Value::Array(vec![Value::from(x / 255.0), Value::from(y / 255.0)]))
            .collect(),
    );
    check_value(parameter, &applied).map_err(|error| error.detail)?;
    Ok(applied)
}

/// Whether `action` is presettable and declares `field`, which an import may write: the check
/// [`transfer_value`] makes for a value it did not convert.
fn writable(registry: &ModuleRegistry, action: &str, field: &str) -> Result<(), String> {
    let (_, descriptor) = registry
        .patch_action(action)
        .map_err(|refusal| refusal.detail)?;
    if descriptor.parameter(field).is_none() {
        return Err(format!("{action} has no {field} field"));
    }
    Ok(())
}

/// Lightroom's `Temperature` and `Tint` converted together to the RAW development's temperature
/// and tint, or why not. `None` when the preset holds neither.
fn raw_white_balance(
    registry: &ModuleRegistry,
    by_name: &HashMap<&str, &RawValue>,
) -> Option<Result<[f64; 2], String>> {
    let (temperature, tint) = (by_name.get("Temperature"), by_name.get("Tint"));
    if temperature.is_none() && tint.is_none() {
        return None;
    }
    Some((|| {
        let (Some(temperature), Some(tint)) = (temperature, tint) else {
            return Err(
                "Lightroom's Temperature and Tint convert together, and the preset holds only one"
                    .to_owned(),
            );
        };
        let number = |value: &RawValue| {
            json_number(value)
                .and_then(|value| value.as_f64())
                .ok_or_else(|| "not a number".to_owned())
        };
        for field in ["temperature", "tint"] {
            writable(registry, RAW, field)?;
        }
        crate::lightroom_to_luxforge(number(temperature)?, number(tint)?)
            .map_err(|error| error.detail)
    })())
}

/// The settings a file carries into Luxforge, and the report of every other setting.
pub(super) struct Mapped {
    pub settings: Map<String, Value>,
    pub report: ImportReport,
}

/// Add one field to a settings set being built.
fn insert(settings: &mut Map<String, Value>, action: &str, field: &str, value: Value) {
    let fields = settings
        .entry(action.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(fields) = fields {
        fields.insert(field.to_owned(), value);
    }
}

/// Map a Lightroom file's settings. A profile's `PresetType` fails the whole import, as does a
/// repeated setting; every other setting lands in exactly one report list or is metadata.
pub(super) fn map(
    format: &str,
    settings: &[RawSetting],
    registry: &ModuleRegistry,
) -> Result<Mapped, Error> {
    let by_name: HashMap<&str, &RawValue> = settings
        .iter()
        .map(|setting| (setting.name.as_str(), &setting.value))
        .collect();
    if by_name.len() != settings.len() {
        let mut seen = std::collections::HashSet::new();
        let repeated = settings
            .iter()
            .find(|setting| !seen.insert(setting.name.as_str()))
            .map_or("", |setting| setting.name.as_str());
        return Err(super::duplicate_setting(repeated));
    }
    if let Some(kind) = by_name.get("PresetType")
        && kind.text().map(str::trim) != Some("Normal")
    {
        return Err(Error::unsupported_input(
            "Lightroom profiles are not presets",
        ));
    }
    let era = era(&by_name);
    let grading = grade_era(format, &by_name);
    // Whether a legacy split-toning document transferred a split-toning setting, so that it also
    // carries the later controls' values (`legacy_split_toning`).
    let mut split_toning = false;
    let auto_tone = by_name.get("AutoTone").and_then(|value| boolean(value)) == Some(true);
    // What the Auto tone step overwrites, as its descriptor declares: the fields a preset
    // carrying Auto does not import.
    let auto_writes = registry
        .action("auto-tone")
        .and_then(|(_, action)| action.analysis.as_ref())
        .map(|analysis| &analysis.writes);
    let switches: HashMap<Panel, Switch> = PANELS
        .iter()
        .map(|panel| {
            let state = match by_name.get(panel.switch()).map(|value| boolean(value)) {
                None | Some(Some(true)) => Switch::On,
                Some(Some(false)) => Switch::Off,
                Some(None) => Switch::Unknown,
            };
            (*panel, state)
        })
        .collect();
    // Lightroom's absolute white balance, converted once for both of its settings.
    let converted = raw_white_balance(registry, &by_name);
    let white_balance = by_name
        .get("WhiteBalance")
        .and_then(|value| value.text())
        .map(str::trim);
    let mut out = Map::new();
    let mut mapped = Vec::new();
    let mut neutral = Vec::new();
    let mut unsupported_list = Vec::new();
    let mut refused = Vec::new();
    let reported = |setting: &RawSetting, reason: Option<&str>| ReportedSetting {
        setting: setting.name.clone(),
        value: report_text(&setting.value),
        reason: reason.map(str::to_owned),
    };
    for setting in settings {
        let name = setting.name.as_str();
        let value = &setting.value;
        if let Some(panel) = PANELS.iter().find(|panel| panel.switch() == name) {
            if switches[panel] == Switch::Unknown {
                unsupported_list.push(reported(setting, Some("panel switch is not a boolean")));
            }
            continue;
        }
        if name.starts_with("Enable") {
            if boolean(value) != Some(true) {
                unsupported_list.push(reported(setting, Some("panel switch not recognised")));
            }
            continue;
        }
        let Some(row) = lookup(name) else {
            unsupported_list.push(reported(setting, Some(NOT_RECOGNISED)));
            continue;
        };
        let switch = row.panel.map_or(Switch::On, |panel| switches[&panel]);
        if switch == Switch::Off {
            match row.rule {
                Rule::Transfer { .. } | Rule::CurveTransfer { .. } => {
                    refused.push(reported(setting, Some(DISABLED)))
                }
                Rule::Metadata => {}
                _ => neutral.push(reported(setting, None)),
            }
            continue;
        }
        match row.rule {
            Rule::Metadata => {}
            Rule::AutoTone => match boolean(value) {
                Some(false) => neutral.push(reported(setting, None)),
                Some(true) => match registry.analysis_action("auto-tone") {
                    Ok(_) => {
                        out.insert("auto-tone".into(), Value::Object(Map::new()));
                        mapped.push(MappedSetting {
                            setting: setting.name.clone(),
                            value: report_text(value),
                            action: "auto-tone".into(),
                            field: None,
                            applied: Value::Object(Map::new()),
                        });
                    }
                    Err(error) => refused.push(reported(setting, Some(&error.detail))),
                },
                None => refused.push(reported(setting, Some("AutoTone is not a boolean"))),
            },
            Rule::Transfer { action, field, .. } | Rule::CurveTransfer { action, field } => {
                if auto_tone
                    && auto_writes
                        .and_then(|writes| writes.get(action))
                        .is_some_and(|written| written.iter().any(|name| name == field))
                {
                    neutral.push(reported(
                        setting,
                        Some("overridden by Auto tone, recomputed for each photo"),
                    ));
                    continue;
                }
                if let Era::Legacy(reason) = &era {
                    refused.push(reported(setting, Some(reason)));
                    continue;
                }
                if switch == Switch::Unknown {
                    let panel = row.panel.map_or("", |panel| panel.switch());
                    let reason = format!("panel switch {panel} is not a boolean");
                    refused.push(reported(setting, Some(&reason)));
                    continue;
                }
                if row.panel == GRADING
                    && let GradeEra::Ambiguous(reason) = &grading
                {
                    refused.push(reported(setting, Some(reason)));
                    continue;
                }
                let applied = match row.rule {
                    Rule::CurveTransfer { .. } => {
                        curve_transfer_value(registry, action, field, value)
                    }
                    _ => transfer_value(registry, action, field, value),
                };
                match applied {
                    Ok(applied) => {
                        split_toning |= row.panel == GRADING && grading == GradeEra::SplitToning;
                        insert(&mut out, action, field, applied.clone());
                        mapped.push(MappedSetting {
                            setting: setting.name.clone(),
                            value: report_text(value),
                            action: action.to_owned(),
                            field: Some(field.to_owned()),
                            applied,
                        });
                    }
                    Err(reason) => refused.push(reported(setting, Some(&reason))),
                }
            }
            Rule::RawWhiteBalance { field } => {
                // Under As Shot the pair is the camera's own white, which As Shot applies.
                if white_balance == Some("As Shot") {
                    neutral.push(reported(setting, None));
                    continue;
                }
                if let Era::Legacy(reason) = &era {
                    refused.push(reported(setting, Some(reason)));
                    continue;
                }
                match &converted {
                    Some(Ok([temperature, tint])) => {
                        let applied = Value::from(if field == "temperature" {
                            *temperature
                        } else {
                            *tint
                        });
                        insert(&mut out, RAW, field, applied.clone());
                        mapped.push(MappedSetting {
                            setting: setting.name.clone(),
                            value: report_text(value),
                            action: RAW.to_owned(),
                            field: Some(field.to_owned()),
                            applied,
                        });
                    }
                    Some(Err(reason)) => refused.push(reported(setting, Some(reason))),
                    None => unreachable!("the preset holds this setting"),
                }
            }
            Rule::WhiteBalance => match value.text().map(str::trim) {
                Some("As Shot") if matches!(era, Era::Legacy(_)) => {
                    let Era::Legacy(reason) = &era else {
                        unreachable!("matched a legacy era")
                    };
                    refused.push(reported(setting, Some(reason)));
                }
                Some("As Shot") => {
                    // The camera's own white balance on a RAW photo, and the file's own rendering
                    // on a JPEG, unless the preset carries a relative correction of its own.
                    let relative = by_name.contains_key("IncrementalTemperature")
                        || by_name.contains_key("IncrementalTint");
                    let mut fields = vec![(RAW, "white-balance", Value::from("as-shot"))];
                    if !relative {
                        fields.push((BASIC, "temperature", Value::from(0.0)));
                        fields.push((BASIC, "tint", Value::from(0.0)));
                    }
                    match fields
                        .iter()
                        .try_for_each(|(action, field, _)| writable(registry, action, field))
                    {
                        Ok(()) => {
                            for (action, field, applied) in fields {
                                insert(&mut out, action, field, applied.clone());
                                mapped.push(MappedSetting {
                                    setting: setting.name.clone(),
                                    value: report_text(value),
                                    action: action.to_owned(),
                                    field: Some(field.to_owned()),
                                    applied,
                                });
                            }
                        }
                        Err(reason) => refused.push(reported(setting, Some(&reason))),
                    }
                }
                Some("Custom") => {
                    let relative = by_name.contains_key("IncrementalTemperature")
                        || by_name.contains_key("IncrementalTint");
                    match &converted {
                        Some(Ok(_)) => neutral.push(reported(setting, None)),
                        None if relative => neutral.push(reported(setting, None)),
                        Some(Err(_)) => refused.push(reported(
                            setting,
                            Some("a custom white balance whose Temperature and Tint are refused"),
                        )),
                        None => refused.push(reported(
                            setting,
                            Some("a custom white balance that names no temperature or tint"),
                        )),
                    }
                }
                _ => refused.push(reported(
                    setting,
                    Some("Luxforge has no Auto or named white balance"),
                )),
            },
            Rule::Profile => {
                if is_default_profile(value) {
                    neutral.push(reported(setting, None));
                } else {
                    unsupported_list.push(reported(setting, Some(NO_PROFILES)));
                }
            }
            Rule::EarlierProcess => match &era {
                Era::Modern => neutral.push(reported(setting, None)),
                Era::Legacy(_) => refused.push(reported(
                    setting,
                    Some("earlier-process field; Luxforge follows Process 2012"),
                )),
            },
            Rule::Unsupported {
                reason,
                neutral: rule,
                unless: condition,
            } => {
                if own_neutral(rule, value) || unless(condition, &by_name) {
                    neutral.push(reported(setting, None));
                } else {
                    unsupported_list.push(reported(setting, Some(reason)));
                }
            }
        }
    }
    // A legacy split-toning document's transferred values mean split toning: Blending 100 and the
    // controls Color Grading added at neutral, one derived entry rather than any setting's. With
    // one of its grading settings refused they would replace the photo's grade without the tint
    // that was meant to replace it, so they are reported and left out.
    let mut derived = Vec::new();
    if split_toning {
        let refusal = refused
            .iter()
            .any(|entry| lookup(&entry.setting).is_some_and(|row| row.panel == GRADING))
            .then(|| SPLIT_TONING_REFUSED.to_owned());
        let mut settings = Map::new();
        for (action, field, applied) in legacy_split_toning() {
            let applied = Value::from(applied);
            if refusal.is_none() {
                insert(&mut out, action, field, applied.clone());
            }
            insert(&mut settings, action, field, applied);
        }
        derived.push(DerivedSettings {
            reason: SPLIT_TONING.to_owned(),
            settings,
            refused: refusal,
        });
    }
    let process_version = by_name
        .get("ProcessVersion")
        .map(|value| report_text(value));
    mapped.sort_by(|a, b| a.setting.cmp(&b.setting));
    for list in [&mut neutral, &mut unsupported_list, &mut refused] {
        list.sort_by(|a, b| a.setting.cmp(&b.setting));
    }
    if !out.is_empty() {
        super::validate_settings(registry, &out).map_err(|error| {
            Error::internal(format!(
                "the importer produced an invalid settings set: {error}"
            ))
        })?;
    }
    Ok(Mapped {
        settings: out,
        report: ImportReport {
            format: format.to_owned(),
            process_version,
            mapped,
            neutral,
            unsupported: unsupported_list,
            refused,
            derived,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_transfer_targets_a_presettable_number_that_covers_the_lightroom_range() {
        let registry = ModuleRegistry::builtin();
        let mut targets = HashSet::new();
        let mut transfers = 0;
        let mut curves = 0;
        for row in ROWS {
            if let Rule::CurveTransfer { action, field } = row.rule {
                curves += 1;
                assert!(
                    targets.insert((action, field)),
                    "{action}.{field} is mapped twice"
                );
                let (_, descriptor) = registry
                    .patch_action(action)
                    .unwrap_or_else(|refusal| panic!("{} targets {refusal}", row.name));
                let parameter = descriptor
                    .parameter(field)
                    .unwrap_or_else(|| panic!("{} targets unknown {action}.{field}", row.name));
                assert!(
                    matches!(
                        parameter.kind,
                        ParameterKind::Curve {
                            points_min: 2,
                            points_max: 16,
                            monotone: true,
                            fixed_x: None,
                        }
                    ),
                    "{action}.{field} is not the free monotone curve of 2..=16 points the \
                     reasons name: {:?}",
                    parameter.kind
                );
                continue;
            }
            let Rule::Transfer {
                action,
                field,
                lightroom: (low, high),
            } = row.rule
            else {
                continue;
            };
            transfers += 1;
            assert!(
                targets.insert((action, field)),
                "{action}.{field} is mapped twice"
            );
            let (_, descriptor) = registry
                .patch_action(action)
                .unwrap_or_else(|refusal| panic!("{} targets {refusal}", row.name));
            let parameter = descriptor
                .parameter(field)
                .unwrap_or_else(|| panic!("{} targets unknown {action}.{field}", row.name));
            let ParameterKind::Number { min, max } = parameter.kind else {
                panic!("{action}.{field} is not a number parameter");
            };
            assert!(
                min <= low && high <= max,
                "{}: {action}.{field} range {min}..{max} does not cover {low}..{high}",
                row.name
            );
        }
        // Basic 10, Presence 3, mixer HSL 24 and grading 14, vignette 4; and the Tone curve's
        // luminance.
        assert_eq!(transfers, 55);
        assert_eq!(curves, 1);
    }

    #[test]
    fn every_name_is_recognized_once_and_every_panel_switch_is_distinct() {
        let mut names = HashSet::new();
        for row in ROWS.iter().chain(PREFIXES) {
            assert!(names.insert(row.name), "{} appears twice", row.name);
            assert!(!row.name.starts_with("Enable"), "{} is a switch", row.name);
        }
        for row in ROWS {
            assert_eq!(lookup(row.name), Some(row), "{}", row.name);
        }
        assert_eq!(
            lookup("SupportsAmount").map(|row| row.rule),
            Some(Rule::Metadata)
        );
        assert_eq!(lookup("Supports"), None);
        assert_eq!(
            lookup("LensProfileName").map(|row| row.name),
            Some("LensProfile")
        );
        assert_eq!(
            lookup("LensProfileEnable").map(|row| row.name),
            Some("LensProfileEnable")
        );
        assert_eq!(
            lookup("UprightVersion").map(|row| row.name),
            Some("Upright")
        );
        assert_eq!(lookup("Unknown"), None);
        let switches: HashSet<_> = PANELS.iter().map(|panel| panel.switch()).collect();
        assert_eq!(switches.len(), PANELS.len());
    }

    #[test]
    fn process_versions_compare_as_numbers() {
        assert_eq!(parse_version("6.7"), Some((6, 7)));
        assert_eq!(parse_version("11.0"), Some((11, 0)));
        assert_eq!(parse_version("15"), Some((15, 0)));
        assert_eq!(parse_version(" 5.7 "), Some((5, 7)));
        for text in ["", "6.", ".7", "+6.7", "6.7.1", "v6", "6,7"] {
            assert_eq!(parse_version(text), None, "{text}");
        }
        // A Camera Raw version may name its point release.
        assert_eq!(parse_camera_raw_version("11.4.1"), Some((11, 4)));
        assert_eq!(parse_camera_raw_version(" 13.0.1 "), Some((13, 0)));
        assert_eq!(parse_camera_raw_version("15.4"), Some((15, 4)));
        assert_eq!(parse_camera_raw_version("16"), Some((16, 0)));
        for text in ["", "11.4.", "11..1", "11.4.1.2", "11.4.x", "+11.4.1", "v11"] {
            assert_eq!(parse_camera_raw_version(text), None, "{text}");
        }
        let text = |value: &str| RawValue::Text(value.to_owned());
        let era_of = |pairs: &[(&'static str, RawValue)]| {
            let map: HashMap<&str, &RawValue> =
                pairs.iter().map(|(name, value)| (*name, value)).collect();
            era(&map)
        };
        assert_eq!(era_of(&[("ProcessVersion", text("6.7"))]), Era::Modern);
        assert_eq!(era_of(&[("ProcessVersion", text("10.0"))]), Era::Modern);
        assert_eq!(
            era_of(&[("ProcessVersion", text("6.6"))]),
            Era::Legacy("earlier process version 6.6".into())
        );
        assert_eq!(
            era_of(&[("ProcessVersion", text("new"))]),
            Era::Legacy("unrecognised process version new".into())
        );
        assert_eq!(era_of(&[("Vibrance", text("5"))]), Era::Modern);
        assert_eq!(
            era_of(&[("Exposure", text("0.5")), ("Vibrance", text("5"))]),
            Era::Legacy("earlier process version".into())
        );
        assert_eq!(
            era_of(&[("Exposure", text("0")), ("Exposure2012", text("0.5"))]),
            Era::Modern
        );
    }

    /// The split-toning fill is the nine controls Color Grading added, on the mixer fields their
    /// own rows transfer to, none of them one of split toning's own.
    #[test]
    fn the_split_toning_fill_covers_every_color_grade_row() {
        let fill: Vec<_> = legacy_split_toning().collect();
        assert_eq!(fill.len(), COLOR_GRADE.len());
        let split: Vec<&str> = ROWS
            .iter()
            .filter(|row| row.name.starts_with("SplitToning"))
            .filter_map(|row| match row.rule {
                Rule::Transfer { field, .. } => Some(field),
                _ => None,
            })
            .collect();
        assert_eq!(split.len(), 5);
        for (action, field, applied) in fill {
            assert_eq!(action, MIXER);
            assert!(!split.contains(&field), "{field}");
            assert_eq!(
                applied,
                if field == "grade-blending" {
                    100.0
                } else {
                    0.0
                }
            );
        }
    }
}
