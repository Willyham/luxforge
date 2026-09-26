//! The one table of Lightroom settings the importer recognizes, and the rules that turn a file's
//! settings into a Lightwell settings set and its report.
//!
//! A mapped value is a **value transfer**: the same number on a Lightwell control with the same
//! name, range and direction, checked against that control's own descriptor. It is not a claim
//! that Lightwell renders what Lightroom renders; `docs/research/lightroom/slider-parity.md` records
//! why equal values do not mean equal pixels.
//! A value that does not parse or lies outside the control's hard range is refused, never clamped.
use super::report::{ImportReport, MappedSetting, ReportedSetting};
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
    /// The named curve is an identity.
    Identity(&'static str),
    /// Every named amount the preset holds is 0, and it holds at least one of them.
    AllZero(&'static [&'static str]),
    /// The named profile is Lightroom's default.
    DefaultProfile(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Rule {
    /// A value transfer onto a presettable Lightwell field. `lightroom` is the range Lightroom
    /// writes, which the target's hard range must cover.
    Transfer {
        action: &'static str,
        field: &'static str,
        lightroom: (f64, f64),
    },
    /// Lightroom's RAW Kelvin temperature or tint: refused until a calibrated conversion exists.
    RawWhiteBalance(&'static str),
    /// `WhiteBalance`: neutral when relative white balance carries it, otherwise refused.
    WhiteBalance,
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
const BASIC: &str = "set-basic";
const PRESENCE: &str = "set-presence";
const MIXER: &str = "set-mixer";
const VIGNETTE: &str = "set-vignette";
const HSL: Option<Panel> = Some(Panel::ColorAdjustments);
const EFFECTS: Option<Panel> = Some(Panel::Effects);
const DETAIL: Option<Panel> = Some(Panel::Detail);
const GRADING: Option<Panel> = Some(Panel::SplitToning);
const LENS: Option<Panel> = Some(Panel::LensCorrections);
const TRANSFORM: Option<Panel> = Some(Panel::Transform);
const CURVE: Option<Panel> = Some(Panel::ToneCurve);
const CALIBRATION: Option<Panel> = Some(Panel::Calibration);

const NO_SHARPENING: &str = "Lightwell has no sharpening";
const NO_NOISE_REDUCTION: &str = "Lightwell has no noise reduction";
const NO_GRAIN: &str = "Lightwell has no grain";
const NO_GRADING: &str = "Lightwell has no colour grading";
const NO_CURVE: &str = "Lightwell has no tone curve";
const NO_GRAYSCALE: &str = "Lightwell has no black-and-white conversion";
const NO_LENS: &str = "Lightwell has no lens corrections";
const NO_CA: &str = "Lightwell has no chromatic-aberration correction";
const NO_LENS_VIGNETTE: &str = "Lightwell has no lens vignetting correction";
const NO_DEFRINGE: &str = "Lightwell has no defringe";
const NO_TRANSFORM: &str = "Lightwell has no upright or perspective correction";
const NO_CALIBRATION: &str = "Lightwell has no calibration";
const NO_PROFILES: &str = "Lightwell has no profiles";
const NO_AUTO_TONE: &str = "Lightwell has no Auto Tone";
const NO_MASKS: &str = "Lightwell has no masks or local corrections";
const NO_SPOTS: &str = "Lightwell has no spot removal";
const NO_RED_EYE: &str = "Lightwell has no red-eye correction";
const NO_MOIRE: &str = "Lightwell has no moiré reduction";
const CROP: &str = "crop belongs to one photo, not to a preset";
const VIGNETTE_STYLE: &str = "Lightwell draws one vignette style";
const VIGNETTE_HIGHLIGHTS: &str = "Lightwell's vignette has no highlight contrast";
pub(super) const NOT_RECOGNISED: &str = "not recognised";
pub(super) const DISABLED: &str = "disabled in the preset";

const PARAMETRIC_REGIONS: &[&str] = &[
    "ParametricShadows",
    "ParametricDarks",
    "ParametricLights",
    "ParametricHighlights",
];
/// Every saturation and luminance amount of colour grading, whose split-toning fields also
/// carry the shadow and highlight wheels.
const GRADE_AMOUNTS: &[&str] = &[
    "SplitToningShadowSaturation",
    "SplitToningHighlightSaturation",
    "ColorGradeMidtoneSat",
    "ColorGradeGlobalSat",
    "ColorGradeShadowLum",
    "ColorGradeMidtoneLum",
    "ColorGradeHighlightLum",
    "ColorGradeGlobalLum",
];

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
        Rule::RawWhiteBalance("RAW Kelvin white balance has no calibrated conversion"),
        None,
    ),
    row(
        "Tint",
        Rule::RawWhiteBalance("RAW tint has no calibrated conversion"),
        None,
    ),
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
    // Tone curves.
    unsupported_row(
        "ToneCurvePV2012",
        NO_CURVE,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ToneCurvePV2012Red",
        NO_CURVE,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ToneCurvePV2012Green",
        NO_CURVE,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ToneCurvePV2012Blue",
        NO_CURVE,
        Identity,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ToneCurveName2012",
        NO_CURVE,
        Neutral::Text("Linear"),
        Unless::Identity("ToneCurvePV2012"),
        CURVE,
    ),
    unsupported_row("ParametricShadows", NO_CURVE, Zero, Unless::Nothing, CURVE),
    unsupported_row("ParametricDarks", NO_CURVE, Zero, Unless::Nothing, CURVE),
    unsupported_row("ParametricLights", NO_CURVE, Zero, Unless::Nothing, CURVE),
    unsupported_row(
        "ParametricHighlights",
        NO_CURVE,
        Zero,
        Unless::Nothing,
        CURVE,
    ),
    unsupported_row(
        "ParametricShadowSplit",
        NO_CURVE,
        Equals(25.0),
        Unless::AllZero(PARAMETRIC_REGIONS),
        CURVE,
    ),
    unsupported_row(
        "ParametricMidtoneSplit",
        NO_CURVE,
        Equals(50.0),
        Unless::AllZero(PARAMETRIC_REGIONS),
        CURVE,
    ),
    unsupported_row(
        "ParametricHighlightSplit",
        NO_CURVE,
        Equals(75.0),
        Unless::AllZero(PARAMETRIC_REGIONS),
        CURVE,
    ),
    unsupported_row(
        "CurveRefineSaturation",
        NO_CURVE,
        Equals(100.0),
        Unless::Nothing,
        CURVE,
    ),
    // Split toning and colour grading.
    unsupported_row(
        "SplitToningShadowHue",
        NO_GRADING,
        Never,
        Unless::Zero("SplitToningShadowSaturation"),
        GRADING,
    ),
    unsupported_row(
        "SplitToningShadowSaturation",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "SplitToningHighlightHue",
        NO_GRADING,
        Never,
        Unless::Zero("SplitToningHighlightSaturation"),
        GRADING,
    ),
    unsupported_row(
        "SplitToningHighlightSaturation",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "SplitToningBalance",
        NO_GRADING,
        Zero,
        Unless::AllZero(GRADE_AMOUNTS),
        GRADING,
    ),
    unsupported_row(
        "ColorGradeMidtoneHue",
        NO_GRADING,
        Never,
        Unless::Zero("ColorGradeMidtoneSat"),
        GRADING,
    ),
    unsupported_row(
        "ColorGradeMidtoneSat",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "ColorGradeGlobalHue",
        NO_GRADING,
        Never,
        Unless::Zero("ColorGradeGlobalSat"),
        GRADING,
    ),
    unsupported_row(
        "ColorGradeGlobalSat",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "ColorGradeShadowLum",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "ColorGradeMidtoneLum",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "ColorGradeHighlightLum",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "ColorGradeGlobalLum",
        NO_GRADING,
        Zero,
        Unless::Nothing,
        GRADING,
    ),
    unsupported_row(
        "ColorGradeBlending",
        NO_GRADING,
        Equals(50.0),
        Unless::AllZero(GRADE_AMOUNTS),
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
    unsupported_row("AutoTone", NO_AUTO_TONE, Off, Unless::Nothing, None),
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

/// Lightroom's default profiles, which Lightwell's own neutral rendering stands in for.
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

/// A preset's process version. Lightwell's controls follow the Process 2012 names, so only a
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

/// `major.minor` as written, without a sign, exponent or anything after the minor digits.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.trim().split_once('.').unwrap_or((text.trim(), "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
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
        Unless::Identity(name) => settings
            .get(name)
            .is_some_and(|value| identity_curve(value)),
        Unless::AllZero(names) => {
            let present: Vec<_> = names.iter().filter_map(|name| settings.get(name)).collect();
            !present.is_empty() && present.into_iter().all(|value| is_zero(Some(value)))
        }
        Unless::DefaultProfile(name) => settings
            .get(name)
            .is_some_and(|value| is_default_profile(value)),
    }
}

/// The value a transfer writes, or why it cannot: the target must be a registered, available
/// field patch, and the value must pass the target parameter's own check.
fn transfer_value(
    registry: &ModuleRegistry,
    action: &str,
    field: &str,
    value: &RawValue,
) -> Result<Value, String> {
    let Some((module, descriptor)) = registry.action(action) else {
        return Err(format!("Lightwell has no {action} action"));
    };
    if !descriptor.patch || !module.descriptor().is_available() {
        return Err(format!("{action} is not available"));
    }
    let Some(parameter) = descriptor.parameter(field) else {
        return Err(format!("{action} has no {field} field"));
    };
    let applied = json_number(value).ok_or_else(|| "not a number".to_owned())?;
    check_value(parameter, &applied).map_err(|_| match &parameter.kind {
        ParameterKind::Number { min, max } => format!("outside Lightwell's range {min}..{max}"),
        ParameterKind::Integer { min, max } => {
            format!("not a whole number within Lightwell's range {min}..{max}")
        }
        _ => "not a value Lightwell accepts".to_owned(),
    })?;
    Ok(applied)
}

/// The settings a file carries into Lightwell, and the report of every other setting.
pub(super) struct Mapped {
    pub settings: Map<String, Value>,
    pub report: ImportReport,
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
                Rule::Transfer { .. } => refused.push(reported(setting, Some(DISABLED))),
                Rule::Metadata => {}
                _ => neutral.push(reported(setting, None)),
            }
            continue;
        }
        match row.rule {
            Rule::Metadata => {}
            Rule::Transfer { action, field, .. } => {
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
                match transfer_value(registry, action, field, value) {
                    Ok(applied) => {
                        let fields = out
                            .entry(action.to_owned())
                            .or_insert_with(|| Value::Object(Map::new()));
                        if let Value::Object(fields) = fields {
                            fields.insert(field.to_owned(), applied.clone());
                        }
                        mapped.push(MappedSetting {
                            setting: setting.name.clone(),
                            value: report_text(value),
                            action: action.to_owned(),
                            field: field.to_owned(),
                            applied,
                        });
                    }
                    Err(reason) => refused.push(reported(setting, Some(&reason))),
                }
            }
            Rule::RawWhiteBalance(reason) => refused.push(reported(setting, Some(reason))),
            Rule::WhiteBalance => {
                let relative = by_name.contains_key("IncrementalTemperature")
                    || by_name.contains_key("IncrementalTint");
                let kelvin = by_name.contains_key("Temperature") || by_name.contains_key("Tint");
                if relative && !kelvin {
                    neutral.push(reported(setting, None));
                } else {
                    refused.push(reported(
                        setting,
                        Some("sets RAW white balance, which no Lightwell field carries"),
                    ));
                }
            }
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
                    Some("earlier-process field; Lightwell follows Process 2012"),
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
        for row in ROWS {
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
            let (module, descriptor) = registry
                .action(action)
                .unwrap_or_else(|| panic!("{} targets unregistered {action}", row.name));
            assert!(descriptor.patch, "{action} is not a field patch");
            assert!(
                module.descriptor().is_available(),
                "{action} is unavailable"
            );
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
        // Basic 10, Presence 3, mixer 24, vignette 4.
        assert_eq!(transfers, 41);
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
}
