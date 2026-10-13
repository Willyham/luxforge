//! A variant's Lightroom Classic settings and the XMP that carries them.
//!
//! Every variant writes the whole baseline, every setting the rounds measure at its neutral value,
//! with its own settings over it. A setting the file leaves out would take the scratch catalog's
//! own default, which for a RAW photograph is not neutral (sharpening 40, colour noise reduction
//! 25, possibly a lens profile) and which a person can change; written out, the variant means the
//! same thing in any catalog.
//!
//! Numbers are written as Lightroom writes them: a signed slider with a leading `+` above zero,
//! Exposure with two decimals, the sharpening radius with one. The preset reader reads each back
//! as the text written.
use std::collections::BTreeMap;

/// One Lightroom setting's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Number {
        value: f64,
        signed: bool,
        decimals: usize,
    },
    Text(&'static str),
    /// A point curve on Lightroom's 0–255 scale, `"x, y"` per point.
    Curve(Vec<[u32; 2]>),
    /// One local correction: a gradient mask and the local exposure it applies.
    Correction(Correction),
}

/// A local correction: one gradient component at full strength, with its local Exposure in stops.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Correction {
    pub mask: Mask,
    pub exposure: f64,
}

/// The stops one unit of `LocalExposure2012` applies. Round 1 measured it from the exports: the
/// written local −1 applied about −5.2 stops where the effect is full, as Lightroom's own global
/// Exposure −4 does on the same grey, so the local value is written as stops over four.
pub const LOCAL_EXPOSURE_STOPS: f64 = 4.0;

/// A gradient component, in Lightroom's normalized image coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mask {
    /// No effect at `zero`, full effect at `full` and beyond.
    Linear { zero: [f64; 2], full: [f64; 2] },
    /// An ellipse in its bounds, rotated by `angle` degrees, the effect intended inside.
    Radial {
        left: f64,
        top: f64,
        right: f64,
        bottom: f64,
        angle: f64,
        feather: f64,
    },
}

impl Mask {
    /// The Lightroom setting that holds a correction of this kind.
    pub fn setting(&self) -> &'static str {
        match self {
            Self::Linear { .. } => "GradientBasedCorrections",
            Self::Radial { .. } => "CircularGradientBasedCorrections",
        }
    }
}

pub fn signed(value: f64) -> Field {
    Field::Number {
        value,
        signed: true,
        decimals: 0,
    }
}

pub fn exposure(value: f64) -> Field {
    Field::Number {
        value,
        signed: true,
        decimals: 2,
    }
}

pub fn radius(value: f64) -> Field {
    Field::Number {
        value,
        signed: true,
        decimals: 1,
    }
}

pub fn unsigned(value: f64) -> Field {
    Field::Number {
        value,
        signed: false,
        decimals: 0,
    }
}

/// A normalized crop coordinate or an angle, with Lightroom's six decimals.
pub fn fraction(value: f64) -> Field {
    Field::Number {
        value,
        signed: false,
        decimals: 6,
    }
}

/// `value` as Lightroom writes a number: `decimals` places, and a `+` above zero when `signed`.
pub fn number_text(value: f64, signed: bool, decimals: usize) -> String {
    let body = format!("{:.*}", decimals, value.abs());
    if value < 0.0 && body.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
        format!("-{body}")
    } else if signed && value > 0.0 {
        format!("+{body}")
    } else {
        body
    }
}

impl Field {
    /// The value as the preset reader reports it: a scalar's text, a curve's points joined with
    /// `; `. A correction is reported as a summary and has none.
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Number {
                value,
                signed,
                decimals,
            } => Some(number_text(*value, *signed, *decimals)),
            Self::Text(text) => Some((*text).to_owned()),
            Self::Curve(points) => Some(
                points
                    .iter()
                    .map(|[x, y]| format!("{x}, {y}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            Self::Correction(_) => None,
        }
    }
}

/// A full set of Lightroom settings, by name, in a stable order.
pub type Fields = BTreeMap<&'static str, Field>;

/// The kinds of photograph a variant is a copy of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A synthetic target, the settings embedded in the JPEG copy.
    Jpeg,
    /// A RAW photograph, the settings in an XMP sidecar beside its copy.
    Raw,
}

/// The colour mixer's Lightroom settings: Hue, Saturation and Luminance of the eight ranges, by
/// kind, then range.
pub const MIXER: [[&str; 8]; 3] = [
    [
        "HueAdjustmentRed",
        "HueAdjustmentOrange",
        "HueAdjustmentYellow",
        "HueAdjustmentGreen",
        "HueAdjustmentAqua",
        "HueAdjustmentBlue",
        "HueAdjustmentPurple",
        "HueAdjustmentMagenta",
    ],
    [
        "SaturationAdjustmentRed",
        "SaturationAdjustmentOrange",
        "SaturationAdjustmentYellow",
        "SaturationAdjustmentGreen",
        "SaturationAdjustmentAqua",
        "SaturationAdjustmentBlue",
        "SaturationAdjustmentPurple",
        "SaturationAdjustmentMagenta",
    ],
    [
        "LuminanceAdjustmentRed",
        "LuminanceAdjustmentOrange",
        "LuminanceAdjustmentYellow",
        "LuminanceAdjustmentGreen",
        "LuminanceAdjustmentAqua",
        "LuminanceAdjustmentBlue",
        "LuminanceAdjustmentPurple",
        "LuminanceAdjustmentMagenta",
    ],
];

/// The identity point curve.
pub fn identity_curve() -> Field {
    Field::Curve(vec![[0, 0], [255, 255]])
}

/// Every setting a round's variant writes, at its neutral value, for a photograph of `kind`:
/// Basic, the tone curves, Presence, the mixer, colour grading, Detail, the vignette, effects,
/// black and white, lens corrections, Transform, crop and calibration. White balance is As Shot on
/// a RAW photograph with Adobe Color, and relative on a JPEG.
pub fn baseline(kind: Kind) -> Fields {
    let mut fields = Fields::new();
    let mut put = |name: &'static str, field: Field| {
        fields.insert(name, field);
    };
    put("Exposure2012", exposure(0.0));
    for name in [
        "Contrast2012",
        "Highlights2012",
        "Shadows2012",
        "Whites2012",
        "Blacks2012",
        "Texture",
        "Clarity2012",
        "Dehaze",
        "Vibrance",
        "Saturation",
    ] {
        put(name, signed(0.0));
    }
    match kind {
        Kind::Jpeg => {
            put("IncrementalTemperature", signed(0.0));
            put("IncrementalTint", signed(0.0));
        }
        Kind::Raw => {
            put("WhiteBalance", Field::Text("As Shot"));
            put("CameraProfile", Field::Text("Adobe Color"));
        }
    }
    for name in [
        "ToneCurvePV2012",
        "ToneCurvePV2012Red",
        "ToneCurvePV2012Green",
        "ToneCurvePV2012Blue",
    ] {
        put(name, identity_curve());
    }
    put("ToneCurveName2012", Field::Text("Linear"));
    for name in [
        "ParametricShadows",
        "ParametricDarks",
        "ParametricLights",
        "ParametricHighlights",
    ] {
        put(name, signed(0.0));
    }
    put("ParametricShadowSplit", unsigned(25.0));
    put("ParametricMidtoneSplit", unsigned(50.0));
    put("ParametricHighlightSplit", unsigned(75.0));
    for name in MIXER.iter().flatten() {
        put(name, signed(0.0));
    }
    for name in [
        "SplitToningShadowHue",
        "SplitToningShadowSaturation",
        "SplitToningHighlightHue",
        "SplitToningHighlightSaturation",
        "ColorGradeMidtoneHue",
        "ColorGradeMidtoneSat",
        "ColorGradeGlobalHue",
        "ColorGradeGlobalSat",
    ] {
        put(name, unsigned(0.0));
    }
    for name in [
        "SplitToningBalance",
        "ColorGradeShadowLum",
        "ColorGradeMidtoneLum",
        "ColorGradeHighlightLum",
        "ColorGradeGlobalLum",
    ] {
        put(name, signed(0.0));
    }
    put("ColorGradeBlending", unsigned(50.0));
    put("Sharpness", unsigned(0.0));
    put("SharpenRadius", radius(1.0));
    put("SharpenDetail", unsigned(25.0));
    put("SharpenEdgeMasking", unsigned(0.0));
    put("LuminanceSmoothing", unsigned(0.0));
    put("LuminanceNoiseReductionDetail", unsigned(50.0));
    put("LuminanceNoiseReductionContrast", unsigned(0.0));
    put("ColorNoiseReduction", unsigned(0.0));
    put("ColorNoiseReductionDetail", unsigned(50.0));
    put("ColorNoiseReductionSmoothness", unsigned(50.0));
    put("PostCropVignetteAmount", signed(0.0));
    put("PostCropVignetteMidpoint", unsigned(50.0));
    put("PostCropVignetteRoundness", signed(0.0));
    put("PostCropVignetteFeather", unsigned(50.0));
    // Highlight Priority, the style Luxforge's vignette follows.
    put("PostCropVignetteStyle", unsigned(1.0));
    put("PostCropVignetteHighlightContrast", unsigned(0.0));
    put("GrainAmount", unsigned(0.0));
    put("ConvertToGrayscale", Field::Text("False"));
    put("LensProfileEnable", unsigned(0.0));
    put("LensManualDistortionAmount", signed(0.0));
    put("AutoLateralCA", unsigned(0.0));
    put("VignetteAmount", signed(0.0));
    put("DefringePurpleAmount", unsigned(0.0));
    put("DefringeGreenAmount", unsigned(0.0));
    put("PerspectiveUpright", unsigned(0.0));
    put("PerspectiveVertical", signed(0.0));
    put("PerspectiveHorizontal", signed(0.0));
    put("PerspectiveRotate", fraction(0.0));
    put("PerspectiveScale", unsigned(100.0));
    put("PerspectiveAspect", signed(0.0));
    put("PerspectiveX", fraction(0.0));
    put("PerspectiveY", fraction(0.0));
    put("HasCrop", Field::Text("False"));
    put("CropTop", fraction(0.0));
    put("CropLeft", fraction(0.0));
    put("CropBottom", fraction(1.0));
    put("CropRight", fraction(1.0));
    put("CropAngle", fraction(0.0));
    put("ShadowTint", signed(0.0));
    for name in [
        "RedHue",
        "RedSaturation",
        "GreenHue",
        "GreenSaturation",
        "BlueHue",
        "BlueSaturation",
    ] {
        put(name, signed(0.0));
    }
    fields
}

/// The settings every XMP carries that describe the file rather than a setting: preset metadata
/// to the reader, never reported.
pub const METADATA: [(&str, &str); 4] = [
    ("Version", "15.4"),
    // Process Version 5, the process string the published research verifies.
    ("ProcessVersion", "11.0"),
    ("HasSettings", "True"),
    ("AlreadyApplied", "False"),
];

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The XMP of `fields`: an `x:xmpmeta` with one `rdf:Description` in the Camera Raw namespace,
/// scalars as attributes, curves and corrections as child elements. `packet` wraps it in the
/// `xpacket` processing instructions an embedded packet carries; a sidecar is the bare document,
/// as Lightroom writes one.
pub fn xmp(fields: &Fields, packet: bool) -> String {
    let mut attributes = String::new();
    for (name, value) in METADATA {
        attributes.push_str(&format!("\n   crs:{name}=\"{value}\""));
    }
    let mut children = String::new();
    for (name, field) in fields {
        match field {
            Field::Number { .. } | Field::Text(_) => {
                let text = field.text().unwrap_or_default();
                attributes.push_str(&format!("\n   crs:{name}=\"{}\"", escape(&text)));
            }
            Field::Curve(points) => {
                children.push_str(&format!("\n   <crs:{name}>\n    <rdf:Seq>"));
                for [x, y] in points {
                    children.push_str(&format!("\n     <rdf:li>{x}, {y}</rdf:li>"));
                }
                children.push_str(&format!("\n    </rdf:Seq>\n   </crs:{name}>"));
            }
            Field::Correction(correction) => {
                children.push_str(&correction_xml(name, correction));
            }
        }
    }
    let document = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF \
         xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description \
         rdf:about=\"\"\n   xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"{attributes}>\
         {children}\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n"
    );
    if packet {
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n{document}<?xpacket \
             end=\"w\"?>"
        )
    } else {
        document
    }
}

fn six(value: f64) -> String {
    number_text(value, false, 6)
}

/// One local correction in the gradient form Lightroom has read since Process 2012: a sequence of
/// one correction holding its local Exposure and one mask component.
fn correction_xml(name: &str, correction: &Correction) -> String {
    let component = match correction.mask {
        Mask::Linear { zero, full } => format!(
            "crs:What=\"Mask/Gradient\" crs:MaskValue=\"1.000000\" crs:ZeroX=\"{}\" \
             crs:ZeroY=\"{}\" crs:FullX=\"{}\" crs:FullY=\"{}\"",
            six(zero[0]),
            six(zero[1]),
            six(full[0]),
            six(full[1])
        ),
        Mask::Radial {
            left,
            top,
            right,
            bottom,
            angle,
            feather,
        } => format!(
            "crs:What=\"Mask/CircularGradient\" crs:MaskValue=\"1.000000\" crs:Top=\"{}\" \
             crs:Left=\"{}\" crs:Bottom=\"{}\" crs:Right=\"{}\" crs:Angle=\"{}\" \
             crs:Midpoint=\"50\" crs:Roundness=\"0\" crs:Feather=\"{}\" crs:Flipped=\"true\" \
             crs:Version=\"2\"",
            six(top),
            six(left),
            six(bottom),
            six(right),
            number_text(angle, false, 0),
            number_text(feather, false, 0)
        ),
    };
    format!(
        "\n   <crs:{name}>\n    <rdf:Seq>\n     <rdf:li>\n      <rdf:Description \
         crs:What=\"Correction\" crs:CorrectionAmount=\"1.000000\" crs:CorrectionActive=\"true\" \
         crs:LocalExposure2012=\"{}\">\n       <crs:CorrectionMasks>\n        <rdf:Seq>\n         \
         <rdf:li {component}/>\n        </rdf:Seq>\n       </crs:CorrectionMasks>\n      \
         </rdf:Description>\n     </rdf:li>\n    </rdf:Seq>\n   </crs:{name}>",
        six(correction.exposure / LOCAL_EXPOSURE_STOPS)
    )
}

/// The corrections an XMP document holds, read back by this rig's own parse: the preset reader
/// reports a correction as unsupported with a summary, so its geometry is checked here.
pub fn corrections(xmp: &str) -> Result<Vec<Correction>, String> {
    const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let body = xmp.trim_start_matches('\u{feff}');
    let document = roxmltree::Document::parse(body).map_err(|error| error.to_string())?;
    let number = |node: roxmltree::Node<'_, '_>, name: &str| -> Result<f64, String> {
        node.attribute((CRS, name))
            .ok_or(format!("a correction has no {name}"))?
            .trim_start_matches('+')
            .parse()
            .map_err(|_| format!("{name} is not a number"))
    };
    let mut out = Vec::new();
    for correction in document.descendants().filter(|node| {
        node.is_element()
            && node.tag_name().namespace() == Some(RDF)
            && node.tag_name().name() == "Description"
            && node.attribute((CRS, "What")) == Some("Correction")
    }) {
        let exposure = number(correction, "LocalExposure2012")? * LOCAL_EXPOSURE_STOPS;
        let component = correction
            .descendants()
            .find(|node| {
                node.is_element()
                    && node
                        .attribute((CRS, "What"))
                        .is_some_and(|what| what.starts_with("Mask/"))
            })
            .ok_or("a correction has no mask component")?;
        let mask = match component.attribute((CRS, "What")) {
            Some("Mask/Gradient") => Mask::Linear {
                zero: [number(component, "ZeroX")?, number(component, "ZeroY")?],
                full: [number(component, "FullX")?, number(component, "FullY")?],
            },
            Some("Mask/CircularGradient") => Mask::Radial {
                left: number(component, "Left")?,
                top: number(component, "Top")?,
                right: number(component, "Right")?,
                bottom: number(component, "Bottom")?,
                angle: number(component, "Angle")?,
                feather: number(component, "Feather")?,
            },
            other => return Err(format!("an unexpected mask component {other:?}")),
        };
        out.push(Correction { mask, exposure });
    }
    Ok(out)
}
