//! What each round measures: its families of variants, the subjects each family is applied to,
//! and the Luxforge control each Lightroom setting is compared with.
//!
//! Round 1 sweeps one setting at a time across its range; round 2 the interacting pairs the design
//! names (`docs/design/lightroom-alignment.md`, "Fitting the translation"); round 3 confirms the
//! realigned sliders at values between round 1's, which no response was fitted to. Colour grading is not
//! here: the delivered `grade-align` round measures it. Brush and range masks wait for a later
//! round (see [`NOT_IN_ROUND`]).
use super::settings::{
    Correction, Field, MIXER, Mask, exposure, fraction, radius, signed, unsigned,
};

/// What a family is applied to: one synthetic target, or every RAW photograph of the round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    Target(&'static str),
    Photographs,
}

/// One variant of a family: its label and the settings it writes over the family's base.
#[derive(Clone, Debug)]
pub struct Step {
    pub label: String,
    pub fields: Vec<(&'static str, Field)>,
}

/// A family: the settings it varies, held over `base`, on every subject in `subjects`.
#[derive(Clone, Debug)]
pub struct Family {
    pub id: String,
    /// The editing area, in the design's order (A2).
    pub area: &'static str,
    pub subjects: Vec<Subject>,
    /// Settings the whole family holds away from neutral so the swept one acts: Sharpening's
    /// Radius is swept at Amount 75.
    pub base: Vec<(&'static str, Field)>,
    pub steps: Vec<Step>,
}

/// The family every subject of a round has: the baseline alone, which every response is measured
/// from.
pub const NEUTRAL: &str = "neutral";

/// Lightroom's signed sliders sampled at ten values, the threshold's ±25 and beyond among them.
pub const SIGNED: [f64; 10] = [
    -100.0, -75.0, -50.0, -25.0, -10.0, 10.0, 25.0, 50.0, 75.0, 100.0,
];
/// Exposure in stops across Lightroom's −5..5.
pub const EXPOSURE: [f64; 10] = [-5.0, -3.0, -2.0, -1.0, -0.5, 0.5, 1.0, 2.0, 3.0, 5.0];
/// An unsigned 0..100 setting whose default is 50.
const AROUND_FIFTY: [f64; 8] = [0.0, 10.0, 25.0, 40.0, 60.0, 75.0, 90.0, 100.0];
/// The coarser grids photographs and the pairs are sampled on.
const COARSE: [f64; 4] = [-100.0, -50.0, 50.0, 100.0];
const COARSE_EXPOSURE: [f64; 4] = [-2.0, -1.0, 1.0, 2.0];

/// A value as a variant label: `+25`, `-0.5`, `0`.
pub fn label(value: f64) -> String {
    let text = if value.fract() == 0.0 {
        format!("{}", value.abs() as i64)
    } else {
        format!("{:.2}", value.abs())
            .trim_end_matches('0')
            .to_owned()
    };
    if value > 0.0 {
        format!("+{text}")
    } else if value < 0.0 {
        format!("-{text}")
    } else {
        text
    }
}

fn sweep(
    id: &str,
    area: &'static str,
    subjects: &[Subject],
    setting: &'static str,
    values: &[f64],
    field: fn(f64) -> Field,
) -> Family {
    held(id, area, subjects, Vec::new(), setting, values, field)
}

fn held(
    id: &str,
    area: &'static str,
    subjects: &[Subject],
    base: Vec<(&'static str, Field)>,
    setting: &'static str,
    values: &[f64],
    field: fn(f64) -> Field,
) -> Family {
    Family {
        id: id.to_owned(),
        area,
        subjects: subjects.to_vec(),
        base,
        steps: values
            .iter()
            .map(|value| Step {
                label: label(*value),
                fields: vec![(setting, field(*value))],
            })
            .collect(),
    }
}

/// Two settings over the product of their values.
#[allow(clippy::too_many_arguments)]
fn pair(
    area: &'static str,
    subjects: &[Subject],
    (first, first_values, first_field): (&'static str, &[f64], fn(f64) -> Field),
    (second, second_values, second_field): (&'static str, &[f64], fn(f64) -> Field),
    first_id: &str,
    second_id: &str,
) -> Family {
    let mut steps = Vec::new();
    for a in first_values {
        for b in second_values {
            steps.push(Step {
                label: format!("{}_{}", label(*a), label(*b)),
                fields: vec![(first, first_field(*a)), (second, second_field(*b))],
            });
        }
    }
    Family {
        id: format!("{first_id}+{second_id}"),
        area,
        subjects: subjects.to_vec(),
        base: Vec::new(),
        steps,
    }
}

const TONE: [(&str, &str); 6] = [
    ("exposure", "Exposure2012"),
    ("contrast", "Contrast2012"),
    ("highlights", "Highlights2012"),
    ("shadows", "Shadows2012"),
    ("whites", "Whites2012"),
    ("blacks", "Blacks2012"),
];

fn tone_field(setting: &str) -> fn(f64) -> Field {
    if setting == "Exposure2012" {
        exposure
    } else {
        signed
    }
}

fn tone_values(setting: &str) -> &'static [f64] {
    if setting == "Exposure2012" {
        &EXPOSURE
    } else {
        &SIGNED
    }
}

fn coarse_values(setting: &str) -> &'static [f64] {
    if setting == "Exposure2012" {
        &COARSE_EXPOSURE
    } else {
        &COARSE
    }
}

use Subject::{Photographs, Target};

/// The point curves of round 1, each on Lightroom's 0–255 scale.
fn curves() -> Vec<(&'static str, Vec<[u32; 2]>)> {
    vec![
        ("midtones+32", vec![[0, 0], [128, 160], [255, 255]]),
        ("midtones+64", vec![[0, 0], [128, 192], [255, 255]]),
        ("midtones-32", vec![[0, 0], [128, 96], [255, 255]]),
        ("midtones-64", vec![[0, 0], [128, 64], [255, 255]]),
        ("s-curve-16", vec![[0, 0], [64, 48], [192, 208], [255, 255]]),
        ("s-curve-32", vec![[0, 0], [64, 32], [192, 224], [255, 255]]),
        ("s-curve-48", vec![[0, 0], [64, 16], [192, 240], [255, 255]]),
        (
            "inverse-s-32",
            vec![[0, 0], [64, 96], [192, 160], [255, 255]],
        ),
        ("black-lift-32", vec![[0, 32], [255, 255]]),
        ("white-pull-32", vec![[0, 0], [255, 223]]),
    ]
}

/// The grid target's size, which a straighten's crop is computed against.
const GRID: (f64, f64) = (1500.0, 1000.0);

/// The crop a straighten of `angle` degrees writes, as `[top, left, bottom, right]`: the largest
/// centred rectangle of the frame's own 3:2 aspect, turned by `angle` (clockwise with y down)
/// about the frame's centre, that stays inside the frame. Round 1's exports showed how Lightroom
/// reads the four values: `(CropLeft, CropTop)` and `(CropRight, CropBottom)` are the turned
/// rectangle's opposite corners in the unturned frame's normalized coordinates, so a box written
/// as an axis-aligned square is a different rectangle for each sign of the angle, and one that
/// leaves the frame is shrunk. Rounded to the six decimals the XMP holds, from a rectangle a
/// ten-thousandth smaller, so the rounding cannot carry a corner out of the frame.
pub fn straighten_box(angle: f64) -> [f64; 4] {
    let (width, height) = GRID;
    let aspect = width / height;
    let (sin, cos) = angle.to_radians().sin_cos();
    let (s, c) = (sin.abs(), cos.abs());
    // Half the rectangle's extent along each frame axis is (w cos + h sin) / 2 and
    // (w sin + h cos) / 2, with h = w / aspect.
    let w = (width / (c + s / aspect)).min(height / (s + c / aspect)) * (1.0 - 1e-4);
    let h = w / aspect;
    // From the centre to the far corner along the rectangle's own axes, turned by `angle`.
    let (dx, dy) = ((w * cos - h * sin) / 2.0, (w * sin + h * cos) / 2.0);
    let six = |value: f64| (value * 1e6).round() / 1e6;
    [
        six((height / 2.0 - dy) / height),
        six((width / 2.0 - dx) / width),
        six((height / 2.0 + dy) / height),
        six((width / 2.0 + dx) / width),
    ]
}

/// The output size Lightroom gives a straighten of `angle` with the crop `[top, left, bottom,
/// right]`, read as its opposite corners: the turned rectangle's width and height in pixels.
#[cfg(test)]
pub fn straighten_size(angle: f64, [top, left, bottom, right]: [f64; 4]) -> (f64, f64) {
    let (width, height) = GRID;
    let (sin, cos) = angle.to_radians().sin_cos();
    let (dx, dy) = ((right - left) * width, (bottom - top) * height);
    (dx * cos + dy * sin, -dx * sin + dy * cos)
}

fn crop(top: f64, left: f64, bottom: f64, right: f64, angle: f64) -> Vec<(&'static str, Field)> {
    vec![
        ("HasCrop", Field::Text("True")),
        ("CropTop", fraction(top)),
        ("CropLeft", fraction(left)),
        ("CropBottom", fraction(bottom)),
        ("CropRight", fraction(right)),
        ("CropAngle", fraction(angle)),
    ]
}

fn geometry() -> Vec<Family> {
    let on = [Target("grid")];
    let straighten = [-10.0, -5.0, -2.0, -1.0, 1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|angle| {
            let [top, left, bottom, right] = straighten_box(angle);
            Step {
                label: label(angle),
                fields: crop(top, left, bottom, right, angle),
            }
        })
        .collect();
    let crops = [
        ("centre", 0.25, 0.25, 0.75, 0.75),
        ("offset", 0.1, 0.3, 0.6, 0.9),
        ("wide", 0.3, 0.05, 0.7, 0.95),
        ("tall", 0.05, 0.4, 0.95, 0.6),
    ]
    .into_iter()
    .map(|(name, top, left, bottom, right)| Step {
        label: name.to_owned(),
        fields: crop(top, left, bottom, right, 0.0),
    })
    .collect();
    vec![
        Family {
            id: "straighten".into(),
            area: "geometry",
            subjects: on.to_vec(),
            base: Vec::new(),
            steps: straighten,
        },
        Family {
            id: "crop".into(),
            area: "geometry",
            subjects: on.to_vec(),
            base: Vec::new(),
            steps: crops,
        },
        sweep(
            "perspective-vertical",
            "geometry",
            &on,
            "PerspectiveVertical",
            &SIGNED,
            signed,
        ),
        sweep(
            "perspective-horizontal",
            "geometry",
            &on,
            "PerspectiveHorizontal",
            &SIGNED,
            signed,
        ),
    ]
}

/// Local Exposure −1 through a linear or radial gradient on the flat field.
fn masks() -> Vec<Family> {
    let correction = |mask: Mask| {
        let field = Field::Correction(Correction {
            mask,
            exposure: -1.0,
        });
        vec![(mask.setting(), field)]
    };
    let linear = [
        ("across", [0.3, 0.5], [0.7, 0.5]),
        ("short", [0.45, 0.5], [0.55, 0.5]),
        ("diagonal", [0.25, 0.25], [0.75, 0.75]),
    ]
    .into_iter()
    .map(|(name, zero, full)| Step {
        label: name.to_owned(),
        fields: correction(Mask::Linear { zero, full }),
    })
    .collect();
    let ellipse = |angle: f64, feather: f64| Mask::Radial {
        left: 0.25,
        top: 0.2,
        right: 0.75,
        bottom: 0.8,
        angle,
        feather,
    };
    let mut radial: Vec<Step> = [0.0, 25.0, 50.0, 75.0, 100.0]
        .into_iter()
        .map(|feather| Step {
            label: format!("feather-{}", label(feather).trim_start_matches('+')),
            fields: correction(ellipse(0.0, feather)),
        })
        .collect();
    radial.push(Step {
        label: "angle-30".into(),
        fields: correction(ellipse(30.0, 50.0)),
    });
    let on = vec![Target("flat")];
    vec![
        Family {
            id: "mask-linear".into(),
            area: "masks",
            subjects: on.clone(),
            base: Vec::new(),
            steps: linear,
        },
        Family {
            id: "mask-radial".into(),
            area: "masks",
            subjects: on,
            base: Vec::new(),
            steps: radial,
        },
    ]
}

fn detail(on: &[Subject], photographs: bool) -> Vec<Family> {
    let at = |setting: &'static str, value: f64| vec![(setting, unsigned(value))];
    if photographs {
        return vec![
            sweep(
                "sharpening",
                "detail",
                on,
                "Sharpness",
                &[40.0, 80.0, 150.0],
                unsigned,
            ),
            sweep(
                "luminance-noise",
                "detail",
                on,
                "LuminanceSmoothing",
                &[25.0, 50.0, 100.0],
                unsigned,
            ),
            sweep(
                "colour-noise",
                "detail",
                on,
                "ColorNoiseReduction",
                &[25.0, 50.0, 100.0],
                unsigned,
            ),
        ];
    }
    let five = [10.0, 25.0, 50.0, 75.0, 100.0];
    vec![
        sweep(
            "sharpening",
            "detail",
            on,
            "Sharpness",
            &[10.0, 25.0, 50.0, 75.0, 100.0, 125.0, 150.0],
            unsigned,
        ),
        held(
            "sharpen-radius",
            "detail",
            on,
            at("Sharpness", 75.0),
            "SharpenRadius",
            &[0.5, 0.8, 1.5, 2.0, 3.0],
            radius,
        ),
        held(
            "sharpen-detail",
            "detail",
            on,
            at("Sharpness", 75.0),
            "SharpenDetail",
            &[0.0, 10.0, 50.0, 75.0, 100.0],
            unsigned,
        ),
        held(
            "sharpen-masking",
            "detail",
            on,
            at("Sharpness", 75.0),
            "SharpenEdgeMasking",
            &five,
            unsigned,
        ),
        sweep(
            "luminance-noise",
            "detail",
            on,
            "LuminanceSmoothing",
            &five,
            unsigned,
        ),
        held(
            "luminance-noise-detail",
            "detail",
            on,
            at("LuminanceSmoothing", 50.0),
            "LuminanceNoiseReductionDetail",
            &[0.0, 25.0, 75.0, 100.0],
            unsigned,
        ),
        sweep(
            "colour-noise",
            "detail",
            on,
            "ColorNoiseReduction",
            &five,
            unsigned,
        ),
        held(
            "colour-noise-detail",
            "detail",
            on,
            at("ColorNoiseReduction", 50.0),
            "ColorNoiseReductionDetail",
            &[0.0, 25.0, 75.0, 100.0],
            unsigned,
        ),
    ]
}

/// RAW white balance: Custom temperatures at tint 0 within the 2000..12000 K Luxforge's RAW
/// development holds, then tints at 5500 K. A pair whose white needs a Luxforge tint beyond ±100 is
/// kept: the importer refuses it, and the manifest records that range shortfall.
fn white_balance() -> Family {
    let mut pairs: Vec<(f64, f64)> = [2500.0, 3200.0, 4000.0, 5000.0, 6500.0, 8000.0, 10000.0]
        .into_iter()
        .map(|temperature| (temperature, 0.0))
        .collect();
    pairs.extend([-50.0, -25.0, -10.0, 0.0, 10.0, 25.0, 50.0].map(|tint| (5500.0, tint)));
    Family {
        id: "white-balance".into(),
        area: "white-balance",
        subjects: vec![Photographs],
        base: Vec::new(),
        steps: pairs
            .into_iter()
            .map(|(temperature, tint)| Step {
                label: format!("{temperature}K{}", label(tint)),
                fields: vec![
                    ("WhiteBalance", Field::Text("Custom")),
                    ("Temperature", unsigned(temperature)),
                    ("Tint", signed(tint)),
                ],
            })
            .collect(),
    }
}

/// Round 1: every supported setting alone across its range.
pub fn round_one() -> Vec<Family> {
    let tone_targets = [Target("wedge"), Target("patches"), Target("surround")];
    let mut families = Vec::new();
    for (id, setting) in TONE {
        families.push(sweep(
            id,
            "basic",
            &tone_targets,
            setting,
            tone_values(setting),
            tone_field(setting),
        ));
    }
    for (id, setting) in [("vibrance", "Vibrance"), ("saturation", "Saturation")] {
        families.push(sweep(
            id,
            "basic",
            &[Target("patches")],
            setting,
            &SIGNED,
            signed,
        ));
    }
    for (id, setting) in [
        ("temperature", "IncrementalTemperature"),
        ("tint", "IncrementalTint"),
    ] {
        families.push(sweep(
            id,
            "basic",
            &[Target("wedge"), Target("patches")],
            setting,
            &SIGNED,
            signed,
        ));
    }
    families.push(Family {
        id: "tone-curve".into(),
        area: "tone-curve",
        subjects: vec![Target("wedge"), Target("patches")],
        base: Vec::new(),
        steps: curves()
            .into_iter()
            .map(|(name, points)| Step {
                label: name.to_owned(),
                fields: vec![
                    ("ToneCurvePV2012", Field::Curve(points)),
                    ("ToneCurveName2012", Field::Text("Custom")),
                ],
            })
            .collect(),
    });
    for (id, setting) in [
        ("texture", "Texture"),
        ("clarity", "Clarity2012"),
        ("dehaze", "Dehaze"),
    ] {
        let mut subjects = vec![Target("edges"), Target("surround")];
        if setting == "Dehaze" {
            subjects.push(Target("patches"));
        }
        families.push(sweep(id, "presence", &subjects, setting, &SIGNED, signed));
        families.push(sweep(
            &format!("{id}-24mp"),
            "presence",
            &[Target("edges-24mp")],
            setting,
            &COARSE,
            signed,
        ));
    }
    for (kind, names) in ["hue", "saturation", "luminance"].into_iter().zip(MIXER) {
        for name in names {
            let colour = name
                .rsplit("Adjustment")
                .next()
                .unwrap_or_default()
                .to_lowercase();
            families.push(sweep(
                &format!("mixer-{colour}-{kind}"),
                "mixer",
                &[Target("patches")],
                name,
                &SIGNED,
                signed,
            ));
        }
    }
    let flat = [Target("flat")];
    let amount = || vec![("PostCropVignetteAmount", signed(-50.0))];
    families.push(sweep(
        "vignette-amount",
        "vignette",
        &flat,
        "PostCropVignetteAmount",
        &SIGNED,
        signed,
    ));
    families.push(held(
        "vignette-midpoint",
        "vignette",
        &flat,
        amount(),
        "PostCropVignetteMidpoint",
        &AROUND_FIFTY,
        unsigned,
    ));
    families.push(held(
        "vignette-roundness",
        "vignette",
        &flat,
        amount(),
        "PostCropVignetteRoundness",
        &SIGNED,
        signed,
    ));
    families.push(held(
        "vignette-feather",
        "vignette",
        &flat,
        amount(),
        "PostCropVignetteFeather",
        &AROUND_FIFTY,
        unsigned,
    ));
    families.extend(detail(&[Target("edges")], false));
    families.extend(geometry());
    families.extend(masks());
    // RAW photographs: what only a real sensor shows.
    families.push(white_balance());
    for (id, setting) in TONE.into_iter().filter(|(id, _)| *id != "contrast") {
        families.push(sweep(
            id,
            "basic",
            &[Photographs],
            setting,
            coarse_values(setting),
            tone_field(setting),
        ));
    }
    families.push(sweep(
        "dehaze",
        "presence",
        &[Photographs],
        "Dehaze",
        &COARSE,
        signed,
    ));
    families.extend(detail(&[Photographs], true));
    families.push(Family {
        id: "lens-profile".into(),
        area: "geometry",
        subjects: vec![Photographs],
        base: Vec::new(),
        steps: vec![Step {
            label: "distortion".into(),
            fields: vec![
                ("LensProfileEnable", unsigned(1.0)),
                ("LensProfileSetup", Field::Text("LensDefaults")),
                ("LensProfileDistortionScale", unsigned(100.0)),
                ("LensProfileVignettingScale", unsigned(0.0)),
            ],
        }],
    });
    distinct(families)
}

/// A family applied to photographs alone, named as one on targets is, takes `-photographs`, so
/// every family of a round has its own id.
fn distinct(mut families: Vec<Family>) -> Vec<Family> {
    let on_targets: Vec<String> = families
        .iter()
        .filter(|family| family.subjects != [Photographs])
        .map(|family| family.id.clone())
        .collect();
    for family in &mut families {
        if family.subjects == [Photographs] && on_targets.contains(&family.id) {
            family.id.push_str("-photographs");
        }
    }
    families
}

/// Round 2: the pairs whose settings interact most, each over the product of four values.
pub fn round_two() -> Vec<Family> {
    let tone_targets = [Target("wedge"), Target("patches"), Target("surround")];
    let setting = |id: &str| -> (&'static str, fn(f64) -> Field) {
        match id {
            "exposure" => ("Exposure2012", exposure),
            "contrast" => ("Contrast2012", signed),
            "highlights" => ("Highlights2012", signed),
            "whites" => ("Whites2012", signed),
            "blacks" => ("Blacks2012", signed),
            "vibrance" => ("Vibrance", signed),
            "saturation" => ("Saturation", signed),
            "clarity" => ("Clarity2012", signed),
            "texture" => ("Texture", signed),
            _ => ("Dehaze", signed),
        }
    };
    let mut families = Vec::new();
    let pairs: [(&str, &str, &[Subject]); 7] = [
        ("exposure", "highlights", &tone_targets),
        ("exposure", "whites", &tone_targets),
        ("contrast", "blacks", &tone_targets),
        ("contrast", "whites", &tone_targets),
        ("vibrance", "saturation", &[Target("patches")]),
        ("clarity", "texture", &[Target("edges"), Target("surround")]),
        (
            "dehaze",
            "contrast",
            &[Target("edges"), Target("patches"), Target("surround")],
        ),
    ];
    for (first, second, subjects) in pairs {
        let (a, a_field) = setting(first);
        let (b, b_field) = setting(second);
        let area = if matches!(first, "clarity" | "dehaze") {
            "presence"
        } else {
            "basic"
        };
        families.push(pair(
            area,
            subjects,
            (a, coarse_values(a), a_field),
            (b, coarse_values(b), b_field),
            first,
            second,
        ));
        if !matches!(first, "vibrance" | "clarity") {
            let extremes = |name: &str| -> &'static [f64] {
                if name == "Exposure2012" {
                    &[-1.0, 1.0]
                } else {
                    &[-100.0, 100.0]
                }
            };
            let mut photographs = pair(
                area,
                &[Photographs],
                (a, extremes(a), a_field),
                (b, extremes(b), b_field),
                first,
                second,
            );
            photographs.id.push_str("-photographs");
            families.push(photographs);
        }
    }
    families
}

/// Round 3's signed values, between round 1's [`SIGNED`].
const BETWEEN: [f64; 10] = [
    -90.0, -60.0, -35.0, -15.0, -5.0, 5.0, 15.0, 35.0, 60.0, 90.0,
];
/// Round 3's Exposure values, between round 1's [`EXPOSURE`].
const BETWEEN_EXPOSURE: [f64; 10] = [-4.0, -2.5, -1.5, -0.75, -0.25, 0.25, 0.75, 1.5, 2.5, 4.0];
/// Round 3's Midpoint values, between round 1's.
const BETWEEN_FIFTY: [f64; 8] = [5.0, 20.0, 33.0, 45.0, 55.0, 67.0, 80.0, 95.0];
/// Round 3's photograph values, between round 1's [`COARSE`].
const BETWEEN_COARSE: [f64; 4] = [-75.0, -35.0, 35.0, 75.0];
const BETWEEN_COARSE_EXPOSURE: [f64; 4] = [-1.5, -0.5, 0.5, 1.5];

/// Round 3, the confirming round: every realigned slider alone at values between round 1's, on
/// the subjects round 1 measured it on, with round 1's family ids so the measurement treats each
/// family as round 1's. Texture, the vignette's Roundness and Feather, Detail, the Tone curve,
/// masks and geometry are not realigned and are not here.
pub fn round_three() -> Vec<Family> {
    let tone_targets = [Target("wedge"), Target("patches"), Target("surround")];
    let between = |setting: &str| -> &'static [f64] {
        if setting == "Exposure2012" {
            &BETWEEN_EXPOSURE
        } else {
            &BETWEEN
        }
    };
    let mut families = Vec::new();
    for (id, setting) in TONE {
        families.push(sweep(
            id,
            "basic",
            &tone_targets,
            setting,
            between(setting),
            tone_field(setting),
        ));
    }
    for (id, setting) in [("vibrance", "Vibrance"), ("saturation", "Saturation")] {
        families.push(sweep(
            id,
            "basic",
            &[Target("patches")],
            setting,
            &BETWEEN,
            signed,
        ));
    }
    for (id, setting) in [
        ("temperature", "IncrementalTemperature"),
        ("tint", "IncrementalTint"),
    ] {
        families.push(sweep(
            id,
            "basic",
            &[Target("wedge"), Target("patches")],
            setting,
            &BETWEEN,
            signed,
        ));
    }
    families.push(sweep(
        "clarity",
        "presence",
        &[Target("edges"), Target("surround")],
        "Clarity2012",
        &BETWEEN,
        signed,
    ));
    families.push(sweep(
        "dehaze",
        "presence",
        &[Target("edges"), Target("surround"), Target("patches")],
        "Dehaze",
        &BETWEEN,
        signed,
    ));
    for (kind, names) in ["hue", "saturation", "luminance"].into_iter().zip(MIXER) {
        for name in names {
            let colour = name
                .rsplit("Adjustment")
                .next()
                .unwrap_or_default()
                .to_lowercase();
            families.push(sweep(
                &format!("mixer-{colour}-{kind}"),
                "mixer",
                &[Target("patches")],
                name,
                &BETWEEN,
                signed,
            ));
        }
    }
    let flat = [Target("flat")];
    families.push(sweep(
        "vignette-amount",
        "vignette",
        &flat,
        "PostCropVignetteAmount",
        &BETWEEN,
        signed,
    ));
    families.push(held(
        "vignette-midpoint",
        "vignette",
        &flat,
        vec![("PostCropVignetteAmount", signed(-50.0))],
        "PostCropVignetteMidpoint",
        &BETWEEN_FIFTY,
        unsigned,
    ));
    for (id, setting) in TONE {
        let values: &[f64] = if setting == "Exposure2012" {
            &BETWEEN_COARSE_EXPOSURE
        } else {
            &BETWEEN_COARSE
        };
        families.push(sweep(
            id,
            "basic",
            &[Photographs],
            setting,
            values,
            tone_field(setting),
        ));
    }
    families.push(sweep(
        "dehaze",
        "presence",
        &[Photographs],
        "Dehaze",
        &BETWEEN_COARSE,
        signed,
    ));
    distinct(families)
}

/// The families of round `round`, or `None` for a round the rig does not define.
pub fn round(round: u32) -> Option<Vec<Family>> {
    match round {
        1 => Some(round_one()),
        2 => Some(round_two()),
        3 => Some(round_three()),
        _ => None,
    }
}

/// What the supported settings' rounds leave out, and why; recorded in every manifest.
pub const NOT_IN_ROUND: [(&str, &str); 3] = [
    (
        "colour grading",
        "measured by the delivered grade-align round, which has its own targets and fits",
    ),
    (
        "brush masks, luminance range and colour range masks",
        "their XMP encodings (dab coordinates and radius, range sampling) are not established from \
         published sources; a later round adds them once a Lightroom-written sidecar of each \
         confirms the encoding",
    ),
    (
        "RAW temperatures above 10000 K",
        "Luxforge's RAW development holds 2000 to 12000 K, so Lightroom's range beyond the sweep \
         is not sampled; tints are sampled past Luxforge's range and recorded as refused",
    ),
];
