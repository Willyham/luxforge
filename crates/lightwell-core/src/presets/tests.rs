//! Importer tests over the synthetic fixtures in `fixtures/presets/` and malformed input. Every
//! fixture asserts the exact settings, the exact report, reasons included, and the origin.
use super::*;
use crate::modules::TestModule;
use crate::{ActionDescriptor, Availability, ModuleDescriptor, ParameterDescriptor, ParameterKind};
use serde_json::json;
use std::time::Instant;

const DEVELOP: &str = include_str!("../../../../fixtures/presets/develop.xmp");
const DEVELOP_PREFIX: &str = include_str!("../../../../fixtures/presets/develop-prefix.xmp");
const PROFILE: &str = include_str!("../../../../fixtures/presets/profile.xmp");
const LEGACY: &str = include_str!("../../../../fixtures/presets/legacy.xmp");
const OUT_OF_RANGE: &str = include_str!("../../../../fixtures/presets/out-of-range.xmp");
const SIDECAR: &str = include_str!("../../../../fixtures/presets/sidecar.xmp");
const FADED: &str = include_str!("../../../../fixtures/presets/faded.lrtemplate");
const SOFT_FILM: &str = include_str!("../../../../fixtures/presets/soft-film.lwpreset");

fn registry() -> ModuleRegistry {
    ModuleRegistry::builtin()
}

fn mapped(setting: &str, value: &str, action: &str, field: &str, applied: Value) -> MappedSetting {
    MappedSetting {
        setting: setting.into(),
        value: value.into(),
        action: action.into(),
        field: field.into(),
        applied,
    }
}

fn neutral(setting: &str, value: &str) -> ReportedSetting {
    ReportedSetting {
        setting: setting.into(),
        value: value.into(),
        reason: None,
    }
}

fn because(setting: &str, value: &str, reason: &str) -> ReportedSetting {
    ReportedSetting {
        setting: setting.into(),
        value: value.into(),
        reason: Some(reason.into()),
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

fn expect_error(result: Result<ImportedPreset, Error>, kind: ErrorKind, detail: &str) {
    let error = result.expect_err("the input is refused");
    assert_eq!(error.kind, kind, "{error}");
    assert!(error.detail.contains(detail), "{error}");
}

const SHARPENING: &str = "Lightwell has no sharpening";
const GRADING: &str = "Lightwell has no colour grading";
const CURVE: &str = "Lightwell has no tone curve";
const PROFILES: &str = "Lightwell has no profiles";
const CROP: &str = "crop belongs to one photo, not to a preset";
const RAW_WB: &str = "sets RAW white balance, which no Lightwell field carries";

fn develop_report(format: &str) -> ImportReport {
    ImportReport {
        format: format.into(),
        process_version: Some("11.0".into()),
        mapped: vec![
            mapped("Blacks2012", "-6", "set-basic", "blacks", json!(-6)),
            mapped("Clarity2012", "+10", "set-presence", "clarity", json!(10)),
            mapped("Contrast2012", "+12", "set-basic", "contrast", json!(12)),
            mapped("Dehaze", "+5", "set-presence", "dehaze", json!(5)),
            mapped(
                "Exposure2012",
                "+0.35",
                "set-basic",
                "exposure",
                json!(0.35),
            ),
            mapped(
                "Highlights2012",
                "-40",
                "set-basic",
                "highlights",
                json!(-40),
            ),
            mapped(
                "HueAdjustmentOrange",
                "0",
                "set-mixer",
                "orange-hue",
                json!(0),
            ),
            mapped("HueAdjustmentRed", "+4", "set-mixer", "red-hue", json!(4)),
            mapped(
                "IncrementalTemperature",
                "+7",
                "set-basic",
                "temperature",
                json!(7),
            ),
            mapped("IncrementalTint", "-3", "set-basic", "tint", json!(-3)),
            mapped(
                "LuminanceAdjustmentGreen",
                "-12.5",
                "set-mixer",
                "green-luminance",
                json!(-12.5),
            ),
            mapped(
                "LuminanceAdjustmentOrange",
                "+6",
                "set-mixer",
                "orange-luminance",
                json!(6),
            ),
            mapped(
                "PostCropVignetteAmount",
                "-18",
                "set-vignette",
                "amount",
                json!(-18),
            ),
            mapped(
                "PostCropVignetteFeather",
                "60",
                "set-vignette",
                "feather",
                json!(60),
            ),
            mapped(
                "PostCropVignetteMidpoint",
                "40",
                "set-vignette",
                "midpoint",
                json!(40),
            ),
            mapped(
                "PostCropVignetteRoundness",
                "0",
                "set-vignette",
                "roundness",
                json!(0),
            ),
            mapped("Saturation", "-5", "set-basic", "saturation", json!(-5)),
            mapped(
                "SaturationAdjustmentBlue",
                "-20",
                "set-mixer",
                "blue-saturation",
                json!(-20),
            ),
            mapped("Shadows2012", "+25", "set-basic", "shadows", json!(25)),
            mapped("Texture", "+15", "set-presence", "texture", json!(15)),
            mapped("Vibrance", "+20", "set-basic", "vibrance", json!(20)),
            mapped("Whites2012", "+8", "set-basic", "whites", json!(8)),
        ],
        neutral: vec![
            neutral("AutoLateralCA", "0"),
            neutral("CameraProfile", "Adobe Standard"),
            neutral("ConvertToGrayscale", "False"),
            neutral("GrainAmount", "0"),
            neutral("GrainFrequency", "50"),
            neutral("GrainSize", "25"),
            neutral("LensProfileEnable", "0"),
            neutral(
                "Look",
                "Name=Adobe Color, Amount=1, UUID=0A1B2C3D4E5F60718293A4B5C6D7E8F9, \
                 SupportsAmount=false, SupportsMonochrome=false, SupportsOutputReferred=false, \
                 Stubbed=true, Group=Profiles",
            ),
            neutral("PostCropVignetteHighlightContrast", "0"),
            neutral("PostCropVignetteStyle", "1"),
            neutral("SplitToningBalance", "0"),
            neutral("SplitToningShadowHue", "220"),
            neutral("SplitToningShadowSaturation", "0"),
            neutral("ToneCurvePV2012Red", "0, 0; 255, 255"),
            neutral("WhiteBalance", "Custom"),
        ],
        unsupported: vec![
            because("SharpenDetail", "25", SHARPENING),
            because("SharpenEdgeMasking", "0", SHARPENING),
            because("SharpenRadius", "+1.0", SHARPENING),
            because("Sharpness", "40", SHARPENING),
            because("SplitToningHighlightHue", "45", GRADING),
            because("SplitToningHighlightSaturation", "12", GRADING),
            because("SyntheticFutureControl", "3", "not recognised"),
            because("ToneCurveName2012", "Custom", CURVE),
            because("ToneCurvePV2012", "0, 0; 64, 56; 192, 200; 255, 255", CURVE),
        ],
        refused: vec![],
    }
}

fn develop_settings() -> Map<String, Value> {
    object(json!({
        "set-basic": {
            "blacks": -6, "contrast": 12, "exposure": 0.35, "highlights": -40,
            "saturation": -5, "shadows": 25, "temperature": 7, "tint": -3,
            "vibrance": 20, "whites": 8
        },
        "set-mixer": {
            "blue-saturation": -20, "green-luminance": -12.5, "orange-hue": 0,
            "orange-luminance": 6, "red-hue": 4
        },
        "set-presence": {"clarity": 10, "dehaze": 5, "texture": 15},
        "set-vignette": {"amount": -18, "feather": 60, "midpoint": 40, "roundness": 0}
    }))
}

#[test]
fn an_xmp_develop_preset_maps_its_values_and_reports_every_other_setting() {
    let preset = parse_preset(DEVELOP, Some("Soft Film.xmp"), &registry()).unwrap();
    assert_eq!(preset.name, "Soft Film");
    assert_eq!(preset.group.as_deref(), Some("Synthetic Looks"));
    assert_eq!(preset.settings, develop_settings());
    assert_eq!(preset.report, develop_report("lightroom-xmp"));
    assert_eq!(
        preset.origin,
        PresetOrigin::LightroomXmp {
            file_name: Some("Soft Film.xmp".into()),
            uuid: Some("5E1F0A2B3C4D4E5F8091A2B3C4D5E6F7".into()),
            process_version: Some("11.0".into()),
            preset_type: Some("Normal".into()),
        }
    );
    assert!(preset.report.partial());
    assert_eq!(
        preset.report.counts(),
        ReportCounts {
            mapped: 22,
            neutral: 15,
            unsupported: 9,
            refused: 0
        }
    );
}

#[test]
fn another_namespace_prefix_imports_exactly_like_crs_and_a_decoy_crs_is_ignored() {
    let expected = parse_preset(DEVELOP, None, &registry()).unwrap();
    let preset = parse_preset(DEVELOP_PREFIX, None, &registry()).unwrap();
    assert_eq!(preset, expected);
}

#[test]
fn a_lightroom_profile_is_refused_as_not_a_preset() {
    for result in [
        parse_preset(PROFILE, Some("Chrome.xmp"), &registry()),
        inspect_preset(PROFILE, Some("Chrome.xmp"), &registry()),
    ] {
        expect_error(
            result,
            ErrorKind::UnsupportedInput,
            "Lightroom profiles are not presets",
        );
    }
}

#[test]
fn an_earlier_process_preset_refuses_every_mapped_value_and_so_maps_nothing() {
    let preset = inspect_preset(LEGACY, Some("legacy.xmp"), &registry()).unwrap();
    let earlier = "earlier-process field; Lightwell follows Process 2012";
    let version = "earlier process version 5.7";
    assert_eq!(preset.name, "Legacy Warm");
    assert_eq!(preset.group, None);
    assert!(preset.settings.is_empty());
    assert_eq!(
        preset.report,
        ImportReport {
            format: "lightroom-xmp".into(),
            process_version: Some("5.7".into()),
            mapped: vec![],
            neutral: vec![],
            unsupported: vec![because("Sharpness", "25", SHARPENING)],
            refused: vec![
                because("AutoExposure", "False", earlier),
                because("Brightness", "+50", earlier),
                because("Clarity", "+15", earlier),
                because("Contrast", "+25", earlier),
                because("Exposure", "+0.50", earlier),
                because("FillLight", "10", earlier),
                because("HighlightRecovery", "20", earlier),
                because("Saturation", "+5", version),
                because("Shadows", "5", earlier),
                because(
                    "ToneCurve",
                    "0, 0; 32, 22; 64, 56; 128, 128; 192, 196; 255, 255",
                    earlier
                ),
                because("ToneCurveName", "Medium Contrast", earlier),
                because("Vibrance", "+10", version),
                because("WhiteBalance", "As Shot", RAW_WB),
            ],
        }
    );
    assert_eq!(
        preset.origin,
        PresetOrigin::LightroomXmp {
            file_name: Some("legacy.xmp".into()),
            uuid: Some("11223344556677889900AABBCCDDEEFF".into()),
            process_version: Some("5.7".into()),
            preset_type: Some("Normal".into()),
        }
    );
    let error = parse_preset(LEGACY, Some("legacy.xmp"), &registry()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert_eq!(
        error.detail,
        "the preset has no setting Lightwell can apply (0 mapped, 0 neutral, 1 unsupported, 13 \
         refused)"
    );
}

#[test]
fn out_of_range_and_unparsable_values_are_refused_and_never_clamped() {
    let preset = parse_preset(OUT_OF_RANGE, Some("bright.xmp"), &registry()).unwrap();
    assert_eq!(preset.name, "Too Bright");
    assert_eq!(preset.group, None);
    assert_eq!(
        preset.settings,
        object(json!({
            "set-basic": {"contrast": -100, "saturation": 100, "shadows": 20, "temperature": 15}
        }))
    );
    assert_eq!(
        preset.report,
        ImportReport {
            format: "lightroom-xmp".into(),
            process_version: Some("15.4".into()),
            mapped: vec![
                mapped("Contrast2012", "-100", "set-basic", "contrast", json!(-100)),
                mapped(
                    "IncrementalTemperature",
                    "+15",
                    "set-basic",
                    "temperature",
                    json!(15)
                ),
                mapped("Saturation", "+100", "set-basic", "saturation", json!(100)),
                mapped("Shadows2012", "+20", "set-basic", "shadows", json!(20)),
            ],
            neutral: vec![],
            unsupported: vec![],
            refused: vec![
                because(
                    "Clarity2012",
                    "-100.5",
                    "outside Lightwell's range -100..100"
                ),
                because("Exposure2012", "+6.0", "outside Lightwell's range -5..5"),
                because(
                    "PostCropVignetteMidpoint",
                    "-10",
                    "outside Lightwell's range 0..100"
                ),
                because(
                    "Temperature",
                    "5500",
                    "RAW Kelvin white balance has no calibrated conversion"
                ),
                because("Tint", "+10", "RAW tint has no calibrated conversion"),
                because("Vibrance", "lots", "not a number"),
                because("WhiteBalance", "Custom", RAW_WB),
            ],
        }
    );
    assert_eq!(
        preset.origin,
        PresetOrigin::LightroomXmp {
            file_name: Some("bright.xmp".into()),
            uuid: None,
            process_version: Some("15.4".into()),
            preset_type: Some("Normal".into()),
        }
    );
}

#[test]
fn a_photo_sidecar_imports_like_a_preset_with_its_crop_reported() {
    let preset = parse_preset(SIDECAR, Some("DSC_0001.xmp"), &registry()).unwrap();
    assert_eq!(preset.name, "DSC_0001");
    assert_eq!(preset.group, None);
    assert_eq!(
        preset.settings,
        object(json!({"set-basic": {"exposure": 0.2, "shadows": 10}}))
    );
    let noise = "Lightwell has no noise reduction";
    let lens = "Lightwell has no lens corrections";
    assert_eq!(
        preset.report,
        ImportReport {
            format: "lightroom-xmp".into(),
            process_version: Some("15.4".into()),
            mapped: vec![
                mapped("Exposure2012", "+0.20", "set-basic", "exposure", json!(0.2)),
                mapped("Shadows2012", "+10", "set-basic", "shadows", json!(10)),
            ],
            neutral: vec![
                neutral("CropAngle", "0"),
                neutral("CropConstrainToWarp", "0"),
                neutral("CropLeft", "0"),
                neutral("CropRight", "1"),
                neutral("LuminanceNoiseReductionDetail", "50"),
                neutral("LuminanceSmoothing", "0"),
                neutral("MaskGroupBasedCorrections", ""),
            ],
            unsupported: vec![
                because("CameraProfile", "Camera Standard", PROFILES),
                because("ColorNoiseReduction", "25", noise),
                because("ColorNoiseReductionDetail", "50", noise),
                because("ColorNoiseReductionSmoothness", "50", noise),
                because("CropBottom", "0.95", CROP),
                because("CropTop", "0.05", CROP),
                because("HasCrop", "True", CROP),
                because("LensProfileEnable", "1", lens),
                because("LensProfileSetup", "LensDefaults", lens),
            ],
            refused: vec![
                because(
                    "Temperature",
                    "5450",
                    "RAW Kelvin white balance has no calibrated conversion"
                ),
                because("Tint", "+8", "RAW tint has no calibrated conversion"),
                because("WhiteBalance", "As Shot", RAW_WB),
            ],
        }
    );
    assert_eq!(
        preset.origin,
        PresetOrigin::LightroomXmp {
            file_name: Some("DSC_0001.xmp".into()),
            uuid: None,
            process_version: Some("15.4".into()),
            preset_type: None,
        }
    );
}

#[test]
fn a_template_reads_zstr_curves_nested_tables_and_panel_switches() {
    let preset = parse_preset(FADED, Some("Faded.lrtemplate"), &registry()).unwrap();
    assert_eq!(preset.name, "Faded Matte");
    assert_eq!(preset.group, None);
    assert_eq!(
        preset.settings,
        object(json!({
            "set-basic": {"blacks": 20, "contrast": -15, "exposure": 0.25, "temperature": 5},
            "set-presence": {"clarity": -10},
            "set-vignette": {"amount": -12, "feather": 50, "midpoint": 50, "roundness": 0}
        }))
    );
    let grain = "Lightwell has no grain";
    let disabled = "disabled in the preset";
    assert_eq!(
        preset.report,
        ImportReport {
            format: "lightroom-template".into(),
            process_version: Some("6.7".into()),
            mapped: vec![
                mapped("Blacks2012", "20", "set-basic", "blacks", json!(20)),
                mapped("Clarity2012", "-10", "set-presence", "clarity", json!(-10)),
                mapped("Contrast2012", "-15", "set-basic", "contrast", json!(-15)),
                mapped("Exposure2012", "0.25", "set-basic", "exposure", json!(0.25)),
                mapped(
                    "IncrementalTemperature",
                    "5",
                    "set-basic",
                    "temperature",
                    json!(5)
                ),
                mapped(
                    "PostCropVignetteAmount",
                    "-12",
                    "set-vignette",
                    "amount",
                    json!(-12)
                ),
                mapped(
                    "PostCropVignetteFeather",
                    "50",
                    "set-vignette",
                    "feather",
                    json!(50)
                ),
                mapped(
                    "PostCropVignetteMidpoint",
                    "50",
                    "set-vignette",
                    "midpoint",
                    json!(50)
                ),
                mapped(
                    "PostCropVignetteRoundness",
                    "0",
                    "set-vignette",
                    "roundness",
                    json!(0)
                ),
            ],
            neutral: vec![
                neutral("AutoGrayscaleMix", "true"),
                neutral("ConvertToGrayscale", "false"),
                neutral("RetouchInfo", ""),
                neutral("SplitToningHighlightHue", "40"),
                neutral("SplitToningHighlightSaturation", "18"),
                neutral("ToneCurvePV2012Blue", "0, 0; 255, 255"),
                neutral("WhiteBalance", "As Shot"),
            ],
            unsupported: vec![
                because("CameraProfile", "Camera [Faded]", PROFILES),
                because("GrainAmount", "12", grain),
                because("GrainFrequency", "50", grain),
                because("GrainSize", "25", grain),
                because(
                    "Look",
                    "Amount=0.5, Name=Synthetic Chrome, Parameters=(1 fields)",
                    PROFILES
                ),
                because(
                    "PostCropVignetteStyle",
                    "2",
                    "Lightwell draws one vignette style"
                ),
                because("Sharpness", "25", SHARPENING),
                because("ToneCurveName2012", "Custom \"S\"", CURVE),
                because(
                    "ToneCurvePV2012",
                    "0, 28; 64, 70; 192, 188; 255, 240",
                    CURVE
                ),
            ],
            refused: vec![
                because("HueAdjustmentAqua", "10", disabled),
                because("LuminanceAdjustmentGreen", "0", disabled),
                because("SaturationAdjustmentBlue", "-30", disabled),
            ],
        }
    );
    assert_eq!(
        preset.origin,
        PresetOrigin::LightroomTemplate {
            file_name: Some("Faded.lrtemplate".into()),
            uuid: Some("8A3F5C2E-1B7D-4E90-A6C4-2D9E0F1B3A57".into()),
        }
    );
}

#[test]
fn a_template_name_and_uuid_fall_back_in_order() {
    let template = |head: &str| {
        format!(
            "s = {{ {head} type = \"Develop\", value = {{ settings = {{ Exposure2012 = 1 }}, \
             uuid = \"VALUE-UUID\" }} }}"
        )
    };
    let preset = parse_preset(&template("internalName = \"Inner\","), None, &registry()).unwrap();
    assert_eq!(preset.name, "Inner");
    assert_eq!(
        preset.origin,
        PresetOrigin::LightroomTemplate {
            file_name: None,
            uuid: Some("VALUE-UUID".into())
        }
    );
    let preset = parse_preset(
        &template("title = \"  \", internalName = \"\","),
        Some("dir/Warm.v2.lrtemplate"),
        &registry(),
    )
    .unwrap();
    assert_eq!(preset.name, "Warm.v2");
    let preset = parse_preset(&template(""), None, &registry()).unwrap();
    assert_eq!(preset.name, IMPORTED_PRESET_NAME);
    expect_error(
        parse_preset(
            "s = { type = \"Library\", value = { settings = {} } }",
            None,
            &registry(),
        ),
        ErrorKind::UnsupportedInput,
        "a Library template is not a Develop preset",
    );
    expect_error(
        parse_preset("s = { value = { settings = {} } }", None, &registry()),
        ErrorKind::UnsupportedInput,
        "the template has no Develop type",
    );
    expect_error(
        parse_preset("s = { type = \"Develop\", value = {} }", None, &registry()),
        ErrorKind::UnsupportedInput,
        "the template has no value.settings table",
    );
    expect_error(
        parse_preset(
            "s = { type = \"Develop\", value = { settings = { 1 } } }",
            None,
            &registry(),
        ),
        ErrorKind::UnsupportedInput,
        "value.settings holds an entry without a name",
    );
}

/// A template holding exactly these settings, for the rules the fixtures do not reach.
fn settings_template(settings: &str) -> String {
    format!(
        "s = {{ title = \"T\", type = \"Develop\", value = {{ settings = {{ {settings} }} }} }}"
    )
}

#[test]
fn a_preset_without_process_version_is_legacy_only_when_it_uses_earlier_tone_fields() {
    let legacy = inspect_preset(
        &settings_template("Exposure = 0.5, Brightness = 50, Vibrance = 10"),
        None,
        &registry(),
    )
    .unwrap();
    assert!(legacy.settings.is_empty());
    assert_eq!(legacy.report.process_version, None);
    assert_eq!(
        legacy.report.refused,
        vec![
            because(
                "Brightness",
                "50",
                "earlier-process field; Lightwell follows Process 2012"
            ),
            because(
                "Exposure",
                "0.5",
                "earlier-process field; Lightwell follows Process 2012"
            ),
            because("Vibrance", "10", "earlier process version"),
        ]
    );
    let modern = parse_preset(
        &settings_template("Exposure = 0, Exposure2012 = 0.5, Texture = 3"),
        None,
        &registry(),
    )
    .unwrap();
    assert_eq!(
        modern.settings,
        object(json!({"set-basic": {"exposure": 0.5}, "set-presence": {"texture": 3}}))
    );
    assert_eq!(modern.report.neutral, vec![neutral("Exposure", "0")]);
    let unknown = inspect_preset(
        &settings_template("ProcessVersion = \"next\", Texture = 3"),
        None,
        &registry(),
    )
    .unwrap();
    assert_eq!(
        unknown.report.refused,
        vec![because("Texture", "3", "unrecognised process version next")]
    );
}

#[test]
fn panel_switches_gate_their_panels_and_unknown_switches_are_reported() {
    let preset = inspect_preset(
        &settings_template(
            "ProcessVersion = \"6.7\", EnableEffects = \"maybe\", PostCropVignetteAmount = 10, GrainAmount = 5, \
             EnableToneCurve = false, ToneCurve = { 0, 10, 255, 255 }, ParametricDarks = 20, \
             EnableFuture = false, EnableOther = true, EnableDetail = 1, Sharpness = 0, \
             EnableRetouch = \"False\", RetouchInfo = { \"spot\" }",
        ),
        None,
        &registry(),
    )
    .unwrap();
    assert!(preset.settings.is_empty());
    assert_eq!(
        preset.report,
        ImportReport {
            format: "lightroom-template".into(),
            process_version: Some("6.7".into()),
            mapped: vec![],
            neutral: vec![
                neutral("ParametricDarks", "20"),
                neutral("RetouchInfo", "spot"),
                neutral("Sharpness", "0"),
                neutral("ToneCurve", "0, 10; 255, 255"),
            ],
            unsupported: vec![
                because("EnableEffects", "maybe", "panel switch is not a boolean"),
                because("EnableFuture", "false", "panel switch not recognised"),
                because("GrainAmount", "5", "Lightwell has no grain"),
            ],
            refused: vec![because(
                "PostCropVignetteAmount",
                "10",
                "panel switch EnableEffects is not a boolean"
            )],
        }
    );
}

#[test]
fn qualifying_settings_are_neutral_only_when_the_amount_they_qualify_is() {
    let preset = inspect_preset(
        &settings_template(
            "ColorGradeMidtoneHue = 30, ColorGradeMidtoneSat = 0, ColorGradeBlending = 70, \
             SplitToningBalance = 20, SplitToningShadowSaturation = 0, ColorGradeGlobalLum = 0, \
             ParametricShadowSplit = 30, ParametricShadows = 0, ParametricLights = 5, \
             PerspectiveScale = 100, UprightVersion = 151388160, PerspectiveUpright = 0, \
             GrayMixerRed = -10, VignetteMidpoint = 40, CameraProfileDigest = \"ABC\", \
             CameraProfile = \"Adobe Color\", DefringePurpleHueLo = 30, \
             ToneCurveName2012 = \"Linear\", PostCropVignetteHighlightContrast = 20, \
             PostCropVignetteAmount = 0, MaskGroupBasedCorrections = { { What = \"Mask\" } }, \
             Exposure2012 = 0",
        ),
        None,
        &registry(),
    )
    .unwrap();
    assert_eq!(
        preset.report.neutral,
        vec![
            neutral("CameraProfile", "Adobe Color"),
            neutral("CameraProfileDigest", "ABC"),
            neutral("ColorGradeBlending", "70"),
            neutral("ColorGradeGlobalLum", "0"),
            neutral("ColorGradeMidtoneHue", "30"),
            neutral("ColorGradeMidtoneSat", "0"),
            neutral("ParametricShadows", "0"),
            neutral("PerspectiveScale", "100"),
            neutral("PerspectiveUpright", "0"),
            neutral("PostCropVignetteHighlightContrast", "20"),
            neutral("SplitToningBalance", "20"),
            neutral("SplitToningShadowSaturation", "0"),
            neutral("ToneCurveName2012", "Linear"),
            neutral("UprightVersion", "151388160"),
        ]
    );
    assert_eq!(
        preset.report.unsupported,
        vec![
            because("DefringePurpleHueLo", "30", "Lightwell has no defringe"),
            because(
                "GrayMixerRed",
                "-10",
                "Lightwell has no black-and-white conversion"
            ),
            because(
                "MaskGroupBasedCorrections",
                "What=Mask",
                "Lightwell has no masks or local corrections"
            ),
            because("ParametricLights", "5", CURVE),
            because("ParametricShadowSplit", "30", CURVE),
            because(
                "VignetteMidpoint",
                "40",
                "Lightwell has no lens vignetting correction"
            ),
        ]
    );
    assert_eq!(
        preset.report.mapped,
        vec![
            mapped("Exposure2012", "0", "set-basic", "exposure", json!(0)),
            mapped(
                "PostCropVignetteAmount",
                "0",
                "set-vignette",
                "amount",
                json!(0)
            ),
        ]
    );
}

#[test]
fn a_lightwell_document_maps_every_field_one_to_one_and_is_what_export_writes() {
    let preset = parse_preset(SOFT_FILM, Some("ignored.lwpreset"), &registry()).unwrap();
    let settings = object(json!({
        "set-basic": {"contrast": 12, "exposure": 0.35, "temperature": 0, "tint": 0},
        "set-mixer": {"blue-saturation": -20},
        "set-vignette": {"amount": -18, "feather": 50, "midpoint": 50, "roundness": 0}
    }));
    assert_eq!(preset.name, "Soft film");
    assert_eq!(preset.group.as_deref(), Some("Synthetic"));
    assert_eq!(preset.settings, settings);
    assert_eq!(preset.origin, PresetOrigin::Lightwell {});
    assert_eq!(
        preset.report,
        ImportReport {
            format: "lightwell".into(),
            process_version: None,
            mapped: vec![
                mapped(
                    "set-basic.contrast",
                    "12",
                    "set-basic",
                    "contrast",
                    json!(12)
                ),
                mapped(
                    "set-basic.exposure",
                    "0.35",
                    "set-basic",
                    "exposure",
                    json!(0.35)
                ),
                mapped(
                    "set-basic.temperature",
                    "0",
                    "set-basic",
                    "temperature",
                    json!(0)
                ),
                mapped("set-basic.tint", "0", "set-basic", "tint", json!(0)),
                mapped(
                    "set-mixer.blue-saturation",
                    "-20",
                    "set-mixer",
                    "blue-saturation",
                    json!(-20)
                ),
                mapped(
                    "set-vignette.amount",
                    "-18",
                    "set-vignette",
                    "amount",
                    json!(-18)
                ),
                mapped(
                    "set-vignette.feather",
                    "50",
                    "set-vignette",
                    "feather",
                    json!(50)
                ),
                mapped(
                    "set-vignette.midpoint",
                    "50",
                    "set-vignette",
                    "midpoint",
                    json!(50)
                ),
                mapped(
                    "set-vignette.roundness",
                    "0",
                    "set-vignette",
                    "roundness",
                    json!(0)
                ),
            ],
            neutral: vec![],
            unsupported: vec![],
            refused: vec![],
        }
    );
    assert!(!preset.report.partial());
    let export = export_document("Soft film", Some("Synthetic"), &settings);
    assert_eq!(export.file_name, "Soft film.lwpreset");
    assert_eq!(export.content, SOFT_FILM);
}

#[test]
fn every_import_round_trips_through_an_exported_document() {
    for (text, file_name) in [
        (DEVELOP, "develop.xmp"),
        (OUT_OF_RANGE, "out-of-range.xmp"),
        (SIDECAR, "sidecar.xmp"),
        (FADED, "faded.lrtemplate"),
    ] {
        let imported = parse_preset(text, Some(file_name), &registry()).unwrap();
        let export = export_document(
            &imported.name,
            imported.group.as_deref(),
            &imported.settings,
        );
        let again = parse_preset(&export.content, Some(&export.file_name), &registry()).unwrap();
        assert_eq!(again.name, imported.name, "{file_name}");
        assert_eq!(again.group, imported.group, "{file_name}");
        assert_eq!(again.settings, imported.settings, "{file_name}");
        assert_eq!(again.origin, PresetOrigin::Lightwell {});
        let fields: usize = imported
            .settings
            .values()
            .map(|fields| fields.as_object().map_or(0, Map::len))
            .sum();
        assert_eq!(again.report.mapped.len(), fields, "{file_name}");
    }
    let export = export_document("Plain", None, &develop_settings());
    assert!(!export.content.contains("\"group\""));
    assert_eq!(
        parse_preset(&export.content, None, &registry())
            .unwrap()
            .group,
        None
    );
}

#[test]
fn export_file_names_are_safe_on_every_desktop() {
    for (name, file_name) in [
        ("Soft film", "Soft film.lwpreset"),
        ("a/b\\c:d*e?f\"g<h>i|j", "a_b_c_d_e_f_g_h_i_j.lwpreset"),
        ("tab\there", "tab_here.lwpreset"),
        ("  .hidden. ", "hidden.lwpreset"),
        ("...", "preset.lwpreset"),
        ("", "preset.lwpreset"),
        ("con", "con_.lwpreset"),
        ("COM1.backup", "COM1_.backup.lwpreset"),
        ("COM0", "COM0.lwpreset"),
        ("Console", "Console.lwpreset"),
        ("Écran 夜", "Écran 夜.lwpreset"),
    ] {
        assert_eq!(export_file_name(name), file_name, "{name:?}");
    }
    let long = "夜".repeat(128);
    let file_name = export_file_name(&long);
    assert!(file_name.len() <= MAX_STEM_BYTES + ".lwpreset".len());
    assert_eq!(file_name, format!("{}.lwpreset", "夜".repeat(66)));
}

#[test]
fn validate_settings_accepts_presettable_fields_and_refuses_everything_else() {
    let registry = registry();
    assert!(validate_settings(&registry, &develop_settings()).is_ok());
    for (settings, kind, detail) in [
        (json!({}), ErrorKind::Validation, "names 1 to 16 actions"),
        (
            json!({"set-teleport": {"x": 1}}),
            ErrorKind::Validation,
            "unknown action set-teleport",
        ),
        (
            json!({"Set_Basic": {"exposure": 1}}),
            ErrorKind::Validation,
            "invalid action identity Set_Basic",
        ),
        (
            json!({"transform": {"operation": "rotate-cw"}}),
            ErrorKind::Validation,
            "transform is not a field-patch action",
        ),
        (
            json!({"set-basic": {"exposure": 5.5}}),
            ErrorKind::Validation,
            "parameter exposure must be a number within -5..=5",
        ),
        (
            json!({"set-basic": {"exposure": "1"}}),
            ErrorKind::Validation,
            "parameter exposure must be a number",
        ),
        (
            json!({"set-basic": {"brightness": 10}}),
            ErrorKind::Validation,
            "unknown parameter brightness for action set-basic",
        ),
        (
            json!({"set-basic": {}}),
            ErrorKind::Validation,
            "must be an object of 1 to 64 fields",
        ),
        (
            json!({"set-basic": 1}),
            ErrorKind::Validation,
            "must be an object of 1 to 64 fields",
        ),
    ] {
        let error = validate_settings(&registry, &object(settings.clone())).unwrap_err();
        assert_eq!(error.kind, kind, "{settings}: {error}");
        assert!(error.detail.contains(detail), "{settings}: {error}");
    }
    let many: Map<String, Value> = (0..17)
        .map(|index| (format!("set-a{index}"), json!({"x": 1})))
        .collect();
    let error = validate_settings(&registry, &many).unwrap_err();
    assert!(
        error
            .detail
            .contains("names 1 to 16 actions; this one names 17")
    );
    let wide: Map<String, Value> = (0..65)
        .map(|index| (format!("f{index}"), json!(0)))
        .collect();
    let error = validate_settings(&registry, &object(json!({"set-basic": wide}))).unwrap_err();
    assert!(error.detail.contains("1 to 64 fields"), "{error}");
}

#[test]
fn validate_settings_refuses_an_unavailable_provider() {
    let parameter = ParameterDescriptor {
        name: "amount".into(),
        kind: ParameterKind::Number { min: 0.0, max: 1.0 },
        required: false,
        default: None,
        unit: None,
        step: None,
        precision: None,
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
        notes: "test".into(),
    };
    let descriptor = ModuleDescriptor {
        id: "test.away".into(),
        title: "Away".into(),
        hint: None,
        effects: vec![],
        actions: vec![ActionDescriptor {
            id: "set-away".into(),
            title: "Set away".into(),
            notes: "test".into(),
            summary: None,
            patch: true,
            parameters: vec![parameter],
        }],
        queries: vec![],
        controls: vec![],
        reset: None,
        canvas: None,
        developer: false,
        collapsed: false,
        layout: crate::ModuleLayout::Stacked,
        availability: Availability::Unavailable {
            reason: "not installed".into(),
        },
        ..ModuleDescriptor::default()
    };
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(TestModule::from_descriptor(descriptor))
        .unwrap();
    let error =
        validate_settings(&registry, &object(json!({"set-away": {"amount": 0.5}}))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert_eq!(error.detail, "unavailable module test.away");
}

#[test]
fn a_lightwell_document_is_strict_about_its_shape_and_version() {
    let document = |body: &str| format!("{{\"format\": \"lightwell.preset\", {body}}}");
    let settings = "\"settings\": {\"set-basic\": {\"exposure\": 1}}";
    for (text, kind, detail) in [
        (
            document(&format!("\"version\": 2, \"name\": \"n\", {settings}")),
            ErrorKind::UnsupportedInput,
            "Lightwell preset document version 2 is not supported; this build reads version 1",
        ),
        (
            document(&format!("\"version\": \"1\", \"name\": \"n\", {settings}")),
            ErrorKind::UnsupportedInput,
            "Lightwell preset document version \"1\" is not supported",
        ),
        (
            document(&format!("\"name\": \"n\", {settings}")),
            ErrorKind::UnsupportedInput,
            "has no version",
        ),
        (
            document(&format!(
                "\"version\": 1, \"name\": \"n\", \"extra\": 1, {settings}"
            )),
            ErrorKind::UnsupportedInput,
            "unknown field `extra`",
        ),
        (
            document("\"version\": 1, \"name\": \"n\""),
            ErrorKind::UnsupportedInput,
            "missing field `settings`",
        ),
        (
            document("\"version\": 1, \"name\": \"n\", \"settings\": {\"transform\": {\"x\": 1}}"),
            ErrorKind::Validation,
            "transform is not a field-patch action",
        ),
        (
            document("\"version\": 1, \"name\": \"n\", \"settings\": {}"),
            ErrorKind::Validation,
            "names 1 to 16 actions",
        ),
        (
            "{\"format\": \"lightroom.preset\", \"version\": 1}".to_owned(),
            ErrorKind::UnsupportedInput,
            "not a Lightwell, Lightroom XMP or .lrtemplate preset",
        ),
        (
            "{\"settings\": {}}".to_owned(),
            ErrorKind::UnsupportedInput,
            "not a Lightwell, Lightroom XMP or .lrtemplate preset",
        ),
        (
            "{\"format\": ".to_owned(),
            ErrorKind::UnsupportedInput,
            "malformed JSON",
        ),
        (
            format!("{}{}", "{\"a\":".repeat(200), "1"),
            ErrorKind::ResourceLimit,
            "deeper than 128 levels",
        ),
    ] {
        expect_error(parse_preset(&text, None, &registry()), kind, detail);
    }
    let blank = document(&format!("\"version\": 1, \"name\": \"  \", {settings}"));
    let preset = parse_preset(&blank, Some("Named.lwpreset"), &registry()).unwrap();
    assert_eq!(preset.name, "Named");
}

#[test]
fn malformed_oversized_and_over_deep_input_fails_with_a_structured_error() {
    let registry = registry();
    let truncated = &DEVELOP[..DEVELOP.len() / 2];
    let dtd = format!(
        "<?xml version=\"1.0\"?><!DOCTYPE x:xmpmeta [<!ENTITY e \"x\">]>{}",
        &DEVELOP
    );
    let deep_xml = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">{}{}</x:xmpmeta>",
        "<a>".repeat(MAX_XMP_DEPTH),
        "</a>".repeat(MAX_XMP_DEPTH)
    );
    let many_nodes = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">{}</x:xmpmeta>",
        "<a/>".repeat(MAX_XMP_NODES as usize)
    );
    let deep_lua = format!(
        "s = {}{}",
        "{ a = ".repeat(MAX_TEMPLATE_DEPTH) + "{",
        "}".repeat(MAX_TEMPLATE_DEPTH + 1)
    );
    let many_values = format!("s = {{ {} }}", "0,".repeat(MAX_TEMPLATE_VALUES));
    let oversized = format!("{}{}", SOFT_FILM, " ".repeat(MAX_PRESET_BYTES));
    let no_camera_raw = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
                         xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
                         <rdf:Description xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" \
                         tiff:Make=\"X\"/></rdf:RDF></x:xmpmeta>";
    let neutral_only = settings_template("Sharpness = 0, GrainAmount = 0");
    let cases: Vec<(String, ErrorKind, &str)> = vec![
        (
            truncated.into(),
            ErrorKind::UnsupportedInput,
            "malformed XMP",
        ),
        (dtd, ErrorKind::UnsupportedInput, "may not contain a DTD"),
        (deep_xml, ErrorKind::ResourceLimit, "deeper than 64 levels"),
        (
            many_nodes,
            ErrorKind::ResourceLimit,
            "more than 200000 XML nodes",
        ),
        (
            "<html><body/></html>".into(),
            ErrorKind::UnsupportedInput,
            "not a Lightwell, Lightroom XMP or .lrtemplate preset",
        ),
        (
            no_camera_raw.into(),
            ErrorKind::UnsupportedInput,
            "no Camera Raw settings",
        ),
        (
            "s = { title = @ }".into(),
            ErrorKind::UnsupportedInput,
            "unsupported value at byte 14",
        ),
        (
            "\u{feff}\n s = { title = \"open }".into(),
            ErrorKind::UnsupportedInput,
            "unterminated string at byte 19",
        ),
        (deep_lua, ErrorKind::ResourceLimit, "deeper than 16 levels"),
        (
            many_values,
            ErrorKind::ResourceLimit,
            "more than 100000 values",
        ),
        (oversized, ErrorKind::ResourceLimit, "at most 1048576 bytes"),
        (
            "".into(),
            ErrorKind::UnsupportedInput,
            "not a Lightwell, Lightroom XMP or .lrtemplate preset",
        ),
        (
            "settings = {}".into(),
            ErrorKind::UnsupportedInput,
            "not a Lightwell, Lightroom XMP or .lrtemplate preset",
        ),
        (
            neutral_only,
            ErrorKind::UnsupportedInput,
            "no setting Lightwell can apply (0 mapped, 2 neutral, 0 unsupported, 0 refused)",
        ),
    ];
    for (text, kind, detail) in cases {
        expect_error(parse_preset(&text, None, &registry), kind, detail);
    }
    // A document exactly at the limit is read, not refused for its size.
    let padded = format!(
        "{SOFT_FILM}{}",
        " ".repeat(MAX_PRESET_BYTES - SOFT_FILM.len())
    );
    assert!(parse_preset(&padded, None, &registry).is_ok());
}

#[test]
fn origins_and_reports_serialize_to_the_record_shapes() {
    let origin = PresetOrigin::LightroomXmp {
        file_name: Some("a.xmp".into()),
        uuid: None,
        process_version: Some("11.0".into()),
        preset_type: None,
    };
    let value = serde_json::to_value(&origin).unwrap();
    assert_eq!(
        value,
        json!({"kind": "lightroom-xmp", "file_name": "a.xmp", "process_version": "11.0"})
    );
    assert_eq!(
        serde_json::from_value::<PresetOrigin>(value).unwrap(),
        origin
    );
    assert_eq!(
        serde_json::to_value(PresetOrigin::Lightwell {}).unwrap(),
        json!({"kind": "lightwell"})
    );
    assert_eq!(
        serde_json::to_value(PresetOrigin::LightroomTemplate {
            file_name: None,
            uuid: Some("u".into())
        })
        .unwrap(),
        json!({"kind": "lightroom-template", "uuid": "u"})
    );
    assert!(
        serde_json::from_value::<PresetOrigin>(json!({"kind": "lightwell", "extra": 1})).is_err()
    );
    assert!(serde_json::from_value::<PresetOrigin>(json!({"kind": "dng"})).is_err());
    assert!(
        serde_json::from_value::<PresetOrigin>(json!({"kind": "lightroom-xmp", "extra": 1}))
            .is_err()
    );
    let report = parse_preset(OUT_OF_RANGE, None, &registry())
        .unwrap()
        .report;
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["format"], "lightroom-xmp");
    assert_eq!(value["process_version"], "15.4");
    assert_eq!(
        value["mapped"][0],
        json!({"setting": "Contrast2012", "value": "-100", "action": "set-basic",
               "field": "contrast", "applied": -100})
    );
    assert_eq!(
        value["refused"][5],
        json!({"setting": "Vibrance", "value": "lots", "reason": "not a number"})
    );
    assert_eq!(
        serde_json::from_value::<ImportReport>(value).unwrap(),
        report
    );
    let neutral_entry = serde_json::to_value(neutral("GrainAmount", "0")).unwrap();
    assert_eq!(
        neutral_entry,
        json!({"setting": "GrainAmount", "value": "0"})
    );
    assert_eq!(
        serde_json::to_value(report.counts()).unwrap(),
        json!({"mapped": 4, "neutral": 0, "unsupported": 0, "refused": 7})
    );
}

/// The worst-case parse time for a document at the request limit, printed rather than asserted:
/// timing gates do not belong in the test suite. Each shape presses one bound: attributes on one
/// element, namespaces in scope, nested structures, template entries and template values.
///
/// ```text
/// cargo test --release --package lightwell-core --lib measure_preset_parse -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement, not a gate"]
fn measure_preset_parse_at_the_size_limit() {
    let registry = registry();
    let fill = |head: &str, tail: &str, entry: &dyn Fn(usize) -> String| {
        let mut text = head.to_owned();
        let mut index = 0;
        loop {
            let next = entry(index);
            if text.len() + next.len() + tail.len() > MAX_PRESET_BYTES {
                break;
            }
            text.push_str(&next);
            index += 1;
        }
        text.push_str(tail);
        (text, index)
    };
    const RDF: &str = "xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"";
    const CRS: &str = "xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"";
    let description = |attributes: usize| {
        let mut text = format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF {RDF}><rdf:Description {CRS} \
             crs:ProcessVersion=\"15.4\" crs:Exposure2012=\"+0.35\""
        );
        for index in 0..attributes {
            text.push_str(&format!(" crs:S{index}=\"+{index}.5\""));
        }
        text
    };
    let close = "</rdf:Description></rdf:RDF></x:xmpmeta>";
    let point = |index: usize| format!("<rdf:li>{}, {}</rdf:li>", index % 256, (index + 1) % 256);
    let mut spare = String::new();
    for index in 0..MAX_XMP_NAMESPACES - 3 {
        spare.push_str(&format!(" xmlns:n{index}=\"urn:n{index}\""));
    }
    let shapes = [
        // Every attribute on one element: the prescan refuses it before the parser's quadratic
        // duplicate check runs.
        (
            "XMP, unbounded attributes on one element",
            fill(&description(0), "/></rdf:RDF></x:xmpmeta>", &|index| {
                format!(" crs:S{index}=\"+{index}.5\"")
            }),
        ),
        // One element at the attribute-pair budget, 1,999 attributes with its namespace
        // declaration, and the rest of the file one curve.
        (
            "XMP, 1,999 attributes on one element and a curve",
            fill(
                &format!("{}><crs:ToneCurvePV2012><rdf:Seq>", description(1996)),
                &format!("</rdf:Seq></crs:ToneCurvePV2012>{close}"),
                &point,
            ),
        ),
        // Just under the node limit, then padding to the size limit.
        (
            "XMP, 199,000 empty elements",
            fill(
                &format!("{}><crs:ToneCurvePV2012><rdf:Seq>", description(0)),
                &format!("</rdf:Seq></crs:ToneCurvePV2012>{close}"),
                &|index| {
                    if index < 199_000 {
                        "<i/>".to_owned()
                    } else {
                        " ".repeat(64)
                    }
                },
            ),
        ),
        // Every element resolves its prefix past the most namespace declarations allowed.
        (
            "XMP, 128 namespaces in scope and a curve",
            fill(
                &format!(
                    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"{spare}><rdf:RDF {RDF}><rdf:Description \
                     {CRS} crs:ProcessVersion=\"15.4\" crs:Exposure2012=\"+0.35\">\
                     <crs:ToneCurvePV2012><rdf:Seq>"
                ),
                &format!("</rdf:Seq></crs:ToneCurvePV2012>{close}"),
                &point,
            ),
        ),
        // A heavily retouched sidecar: many nested structures of 40 fields each.
        (
            "XMP, nested structures of 40 fields",
            fill(
                &format!("{}><crs:RetouchAreas><rdf:Seq>", description(0)),
                &format!("</rdf:Seq></crs:RetouchAreas>{close}"),
                &|index| {
                    let mut item = "<rdf:li><rdf:Description".to_owned();
                    for field in 0..40 {
                        item.push_str(&format!(" crs:F{field}=\"{index}\""));
                    }
                    item + "/></rdf:li>"
                },
            ),
        ),
        (
            "template, unrecognised settings",
            fill(
                "s = { title = \"Big\", type = \"Develop\", value = { settings = { \
                 ProcessVersion = \"6.7\", Exposure2012 = 0.35, ",
                "}, }, }",
                &|index| format!("S{index} = {index}.5, "),
            ),
        ),
        // A flat curve just inside the value limit, padded with spaces to the size limit.
        (
            "template, one flat curve at the value limit",
            fill(
                "s = { title = \"Big\", type = \"Develop\", value = { settings = { \
                 ProcessVersion = \"6.7\", Exposure2012 = 0.35, ToneCurvePV2012 = { ",
                "}, }, }, }",
                &|index| {
                    if index < MAX_TEMPLATE_VALUES - 16 {
                        format!("{}, ", index % 256)
                    } else {
                        " ".repeat(64)
                    }
                },
            ),
        ),
    ];
    for (label, (text, count)) in shapes {
        let preset = inspect_preset(&text, None, &registry);
        let outcome = match &preset {
            Ok(preset) => format!("{}", preset.report.counts()),
            Err(error) => format!("{error}"),
        };
        let mut samples = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            let _ =
                std::hint::black_box(inspect_preset(std::hint::black_box(&text), None, &registry));
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "{label}: {} bytes, {count} entries; p50 {:.2} ms, max {:.2} ms over 20 runs; {outcome}",
            text.len(),
            samples[samples.len() / 2],
            samples[samples.len() - 1],
        );
    }
}
