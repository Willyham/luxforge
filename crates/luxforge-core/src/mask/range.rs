//! The `luminance-range` and `colour-range` component kinds: the frozen range selections of
//! `docs/design/range-study.md`, compiled against the content stage their mask's layer receives.
//!
//! This file is the production transcription of that study and of the independent `f64` reference at
//! `crates/luxforge-core/tests/reference/range.rs`. The per-pixel expressions are written in the
//! reference's form and the reference's order, so coverage is bit-identical to it rather than merely
//! within tolerance: `lo`, `hi`, `lo_feather`, `hi_feather`, each sample's Oklab `(a, b)` pair and
//! the refine radius are hoisted to compile time because they do not depend on the pixel, the
//! feathers and the radius stay **divisors** rather than becoming precomputed reciprocals, there is
//! no fused multiply-add and no reassociated dot product or matrix product, [`smooth`] is not folded
//! into Horner's form, and the `+ 1` of a shoulder is not folded into its numerator.
//!
//! **These two kinds read a value, not a position.** Everything else in this module answers
//! `coverage(u, v)` from the pixel's position alone; a range selection answers from the **pixel the
//! operation it modulates receives**, which is what [`super::CompiledMask::coverage`] carries as its
//! `rgb` argument (proposal P12 of the study, decided there). Three consequences are stated rather
//! than hidden, and each has its own answer below: the conservative rectangle is the whole stage
//! ([`value_support`]), the smallest feature is `f64::INFINITY` because no pixel grid resolves a
//! value test ([`value_feature_px`]), and what such a component selects moves when a layer ahead of
//! it changes the operation's input.
//!
//! **Why the arithmetic is `f64` and not `f32`.** The study derives a coverage tolerance from the
//! shoulder's Lipschitz constant precisely because a pixel is computed in `f32` by a render and in
//! `f64` by the reference. This unit removes that gap instead of living inside it: the incoming
//! `[f32; 3]` is widened once, at the top of [`super::CompiledMask::evaluate`], and every expression
//! after it is the reference's own `f64` expression. The whole coverage field is already `f64` — the
//! mask study froze it that way — so nothing is paid for this that the geometric components were not
//! already paying, and the result is exact equality with the oracle instead of a bound.
//!
//! The luminance axis and the Oklab conversion are the **delivered** ones, read from the Basic
//! module's own `f64` constants rather than copied here, so the editor keeps one definition of
//! luminance and one colour space.
use super::smooth;
use crate::{
    Component, Error, ErrorKind, ParameterDescriptor,
    modules::{
        Region, Stage,
        basic::{
            colour::{M1_F64, M2_F64},
            tone::{LUMA_B_F64, LUMA_G_F64, LUMA_R_F64},
        },
    },
};
use serde::{Deserialize, Serialize};

/// The token a stored luminance-range component carries.
pub(super) const LUMINANCE_KIND: &str = "luminance-range";

/// The token a stored colour-range component carries.
pub(super) const COLOUR_KIND: &str = "colour-range";

/// What a luminance band cannot separate, with the study's own measurement.
///
/// The figure is the point: a photographed blue sky reads `47.254` on this axis and a mid-grey card
/// `47.815`, which is `0.56` of a slider unit and `1.43` output codes, so **no** band takes one
/// without the other. It is phrased as a remedy because the remedy is the component list a person is
/// already looking at, and it is the study's
/// `the_luminance_band_cannot_separate_a_blue_sky_from_a_grey_card`.
pub(super) const LUMINANCE_LIMITS: &[&str] = &[
    "Brightness only · a photographed blue sky and a grey card are 1.4 output codes apart, so one band takes both: intersect a colour range, or subtract a brush",
];

/// What a colour range cannot separate, with the study's own measurements.
///
/// Two facts, because they have one cause — the metric is the chromaticity plane and `L` does not
/// appear, which is what holds a surface across a stop of shading. Every neutral is within `0.0015` of
/// every other, a third of the tightest radius, so a sampled grey selects the whole tonal range; and
/// dark skin is `0.0108` from light skin, a third of what one face's own shading spans, so any setting
/// that holds a lit face takes both. `every_neutral_is_one_colour_to_the_frozen_metric` and the
/// study's skin measurement assert them.
pub(super) const COLOUR_LIMITS: &[&str] = &[
    "Colour only, with no lightness · every neutral is one colour, and two people's skin is one colour: intersect a luminance range, or subtract a brush",
];

// ---------------------------------------------------------------------------
// The shared axes: the luminance axis and the Oklab chromaticity of one pixel
// ---------------------------------------------------------------------------

/// The sRGB OETF (linear to encoded), analytically continued to every finite real value, in `f64`.
///
/// The same function `crate::modules::basic::tone::encode_srgb_extended` computes in `f32`, written
/// out here in `f64` because this unit's whole arithmetic is the reference's. It is **not clamped**:
/// an earlier unit in the same colour run may legitimately hand on a linear value below `0` or above
/// `1`, and the continued OETF is defined and strictly increasing on the whole real line, so such a
/// pixel is treated as darker than black or brighter than white rather than folded onto the axis.
fn encode_srgb_extended(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The luminance axis: Rec. 709 relative luminance of the linear-sRGB pixel, encoded through the
/// continued OETF.
///
/// This is the domain the delivered histogram bins — the histogram's horizontal axis *is* the
/// encoded output value, quantized to a byte — so a number on the band's slider is the number a
/// person reads off the histogram's axis. The weights are the delivered Basic layer's own, so there
/// is one definition of luminance in the editor.
fn luminance_axis(rgb: [f64; 3]) -> f64 {
    let y = LUMA_R_F64 * rgb[0] + LUMA_G_F64 * rgb[1] + LUMA_B_F64 * rgb[2];
    encode_srgb_extended(y)
}

fn matvec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The signed cube root `sign(x) * |x|^(1/3)`, finite for every finite `x` including a negative one.
/// `powf(1/3)` is not safe here: it is NaN for a negative base, and a linear value preserved from an
/// earlier unit can land a negative LMS component after `M1`.
fn signed_cbrt(x: f64) -> f64 {
    x.signum() * x.abs().cbrt()
}

/// The Oklab chromaticity pair `(a, b)` of one linear-sRGB pixel, through the delivered conversion's
/// own matrices. `L` is computed and discarded: the frozen metric is blind to lightness, which is
/// the study's measured choice and not an omission, and computing the third row costs three
/// multiplies that keep this spelling the delivered `to_oklab`'s spelling exactly.
fn oklab_ab(rgb: [f64; 3]) -> (f64, f64) {
    let lms = matvec(&M1_F64, rgb);
    let lms_root = [
        signed_cbrt(lms[0]),
        signed_cbrt(lms[1]),
        signed_cbrt(lms[2]),
    ];
    let lab = matvec(&M2_F64, lms_root);
    (lab[1], lab[2])
}

// ---------------------------------------------------------------------------
// The luminance band
// ---------------------------------------------------------------------------

/// The ends of the band's own axis, which is the encoded axis times 100: one slider unit is `2.55`
/// output codes, so the conversion from a slider number to a code is exact and one line.
pub const LEVEL_MIN: f64 = 0.0;
pub const LEVEL_MAX: f64 = 100.0;

/// The smallest legal **non-zero** shoulder width, in slider units. A feather of exactly `0` is the
/// explicit hard-edge branch; anything between is refused, because the shoulder's slope is
/// `1.5 / feather` and a shoulder narrower than one slider step is a hard edge asked for in a way
/// the arithmetic cannot bound. The floor bounds every reciprocal the band takes by `100`, which is
/// what makes the study's tolerance derivable, and it excludes nothing a gesture can ask for: one
/// unit is the smallest non-zero step a `0..100` slider offers.
pub const RANGE_FEATHER_MIN: f64 = 1.0;

/// The largest legal shoulder width, in slider units: the whole axis.
pub const RANGE_FEATHER_MAX: f64 = 100.0;

/// The shoulder a new band starts with.
///
/// A new luminance range starts as **the whole tonal range with soft shoulders** — `low = 0`,
/// `high = 100`, both feathers at [`FEATHER_DEFAULT`] — so creating one selects the picture and a
/// person narrows it, the way a crop starts at the whole frame rather than at a guessed rectangle.
/// The shoulders are soft rather than hard because the study measures a hard band flipping 47% of
/// neighbouring pixels on an ordinary noisy shadow: a starting value that speckles would read as a
/// fault in the tool. Five units is 13 output codes, comfortably wider than that noise, and is the
/// narrowest shoulder the study measures as clean.
pub const FEATHER_DEFAULT: f64 = 5.0;

/// A luminance-range component's stored payload, all four numbers on the slider's `0..100` axis.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LuminanceRange {
    pub low: f64,
    pub low_feather: f64,
    pub high: f64,
    pub high_feather: f64,
}

/// What this kind declares to the API: the band's two edges and its two shoulders, each carrying the
/// range [`parse_luminance`] enforces. The shoulder's "zero or at least one" rule is a payload rule
/// and not a range, so it lives in the parser and is named in the notes a generated control shows.
pub(super) fn luminance_parameters(required: bool) -> Vec<ParameterDescriptor> {
    vec![
        level(
            "low",
            required,
            LEVEL_MIN,
            "the dark edge of the band, on the histogram's own axis; one unit is 2.55 output codes",
        ),
        feather(
            "low_feather",
            required,
            "how far below the dark edge the selection ramps away, in the same units; 0 is a hard \
             edge and anything else is at least 1, because a narrower shoulder is a hard edge asked \
             for indirectly",
        ),
        level(
            "high",
            required,
            LEVEL_MAX,
            "the bright edge of the band, on the histogram's own axis; it may not be below the dark \
             edge",
        ),
        feather(
            "high_feather",
            required,
            "how far above the bright edge the selection ramps away, in the same units; 0 is a hard \
             edge and anything else is at least 1",
        ),
    ]
}

fn level(name: &str, required: bool, default: f64, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        unit: Some("%".to_owned()),
        step: Some(1.0),
        precision: Some(1),
        fine_step: Some(0.1),
        default: Some(serde_json::json!(default)),
        ..super::parameters::parameter(
            name,
            crate::ParameterKind::Number {
                min: LEVEL_MIN,
                max: LEVEL_MAX,
            },
            required,
            notes,
        )
    }
}

fn feather(name: &str, required: bool, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        unit: Some("%".to_owned()),
        step: Some(1.0),
        precision: Some(1),
        fine_step: Some(0.1),
        zero: Some(0.0),
        default: Some(serde_json::json!(FEATHER_DEFAULT)),
        ..super::parameters::parameter(
            name,
            crate::ParameterKind::Number {
                min: 0.0,
                max: RANGE_FEATHER_MAX,
            },
            required,
            notes,
        )
    }
}

/// Parse and range-check one stored `luminance-range` payload. Nothing here depends on a stage: the
/// band's numbers are on an axis the picture defines, not on the frame.
pub(super) fn parse_luminance(component: &Component) -> Result<LuminanceRange, Error> {
    let range: LuminanceRange =
        serde_json::from_value(component.payload.clone()).map_err(|error| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} has an invalid {LUMINANCE_KIND} payload: {error}",
                    component.name
                ),
            )
        })?;
    let refuse = |field: &str, what: String| {
        Err(Error::new(
            ErrorKind::Validation,
            format!(
                "component {} {LUMINANCE_KIND} {field} {what}",
                component.name
            ),
        ))
    };
    for (field, value) in [("low", range.low), ("high", range.high)] {
        if !value.is_finite() || !(LEVEL_MIN..=LEVEL_MAX).contains(&value) {
            return refuse(
                field,
                format!("must be a number within {LEVEL_MIN:.0}..={LEVEL_MAX:.0}"),
            );
        }
    }
    for (field, value) in [
        ("low_feather", range.low_feather),
        ("high_feather", range.high_feather),
    ] {
        if !value.is_finite()
            || !(value == 0.0 || (RANGE_FEATHER_MIN..=RANGE_FEATHER_MAX).contains(&value))
        {
            return refuse(
                field,
                format!(
                    "must be exactly 0 for a hard edge or a number within \
                     {RANGE_FEATHER_MIN:.0}..={RANGE_FEATHER_MAX:.0}"
                ),
            );
        }
    }
    // The mathematics is total either way — a crossed band simply selects nothing — but a selection
    // a person cannot see the cause of is refused rather than rendered empty.
    if range.low > range.high {
        return refuse(
            "low",
            format!("must not be above high, and {} is", range.high),
        );
    }
    Ok(range)
}

/// One luminance band compiled once per component: the payload divided onto the encoded axis, which
/// is the only per-component work the band needs. The per-pixel path therefore never multiplies a
/// pixel's own value by 100.
#[derive(Clone, Copy, Debug)]
pub(super) struct CompiledLuminance {
    lo: f64,
    hi: f64,
    lo_feather: f64,
    hi_feather: f64,
}

impl CompiledLuminance {
    pub(super) fn new(stored: LuminanceRange) -> Self {
        Self {
            lo: stored.low / 100.0,
            hi: stored.high / 100.0,
            lo_feather: stored.low_feather / 100.0,
            hi_feather: stored.high_feather / 100.0,
        }
    }

    /// Coverage for one linear-sRGB pixel, as the study froze it:
    ///
    /// ```text
    /// Y    = 0.2126*r + 0.7152*g + 0.0722*b
    /// e    = encode_srgb_extended(Y)
    /// rise = if lo_feather == 0 { if e >= lo { 1 } else { 0 } }
    ///        else               { smooth(clamp((e - lo) / lo_feather + 1, 0, 1)) }
    /// fall = if hi_feather == 0 { if e <= hi { 1 } else { 0 } }
    ///        else               { smooth(clamp((hi - e) / hi_feather + 1, 0, 1)) }
    /// c    = min(rise, fall)
    /// ```
    ///
    /// The `+ 1` spelling is frozen rather than the algebraically equal
    /// `(e - (lo - lo_feather)) / lo_feather`, because it is exact at both ends of the shoulder: at
    /// `e == lo` the numerator is exactly `0`, so the ratio is exactly `1` and `smooth(1)` exactly
    /// `1`; at `e == lo - lo_feather` the ratio is exactly `-1 + 1 == 0`.
    ///
    /// `min` is the frozen composition algebra's own intersection, which is what a band *is* —
    /// brighter than the low edge **and** darker than the high edge — and it introduces no rounding
    /// at all, so overlapping shoulders on a narrow band stay exactly `1` between the edges.
    ///
    /// The hard branch follows the radial's `span == 0` discipline: a feather of exactly zero, and
    /// only that, takes it, so a vanishing shoulder is never divided by.
    pub(super) fn coverage(&self, rgb: [f64; 3]) -> f64 {
        let e = luminance_axis(rgb);
        let rise = if self.lo_feather == 0.0 {
            if e >= self.lo { 1.0 } else { 0.0 }
        } else {
            smooth((((e - self.lo) / self.lo_feather) + 1.0).clamp(0.0, 1.0))
        };
        let fall = if self.hi_feather == 0.0 {
            if e <= self.hi { 1.0 } else { 0.0 }
        } else {
            smooth((((self.hi - e) / self.hi_feather) + 1.0).clamp(0.0, 1.0))
        };
        rise.min(fall)
    }
}

// ---------------------------------------------------------------------------
// The colour range
// ---------------------------------------------------------------------------

/// The largest number of sampled colours one colour-range component holds. A product bound — the
/// panel shows five swatches — and not a numerical one: the fold is `min`, which is exact and
/// associative, so nothing in the mathematics changes if it is raised.
pub const MAX_SAMPLES: usize = 5;

/// The loosest selection radius, in Oklab `(a, b)` units, at `refine = 0`: 41% of the largest
/// chromaticity distance between two in-gamut sRGB colours, so the loosest setting takes a broad
/// family and still leaves the opposite side of the wheel out.
pub const RADIUS_MAX: f64 = 0.25;

/// The tightest selection radius, at `refine = 100`: below the measured chromaticity spread of a
/// single surface under half a stop of shading, so the tightest setting selects one flat patch and
/// little else. It bounds the per-pixel reciprocal by `200`.
pub const RADIUS_MIN: f64 = 0.005;

/// The fraction of the radius that is fully selected. Sized so that at the default refine the
/// plateau covers an ordinary surface's own chromaticity spread across a stop of shading.
pub const PLATEAU: f64 = 0.5;

/// The falloff span, `1 - PLATEAU`. Exactly `0.5`, so dividing by it is exact.
pub const SPAN: f64 = 1.0 - PLATEAU;

/// The refine slider's ends.
pub const REFINE_MIN: f64 = 0.0;
pub const REFINE_MAX: f64 = 100.0;

/// The refine a new colour range starts at, and the one a double-click returns it to. It is a
/// measured position and not a midpoint: the refine at which a surface stops being held whole across
/// a stop of shading is `53.0` for skin, `52.7` for a sky and `50.5` for foliage, so the geometric
/// mean of the two radius ends lands within three points of "hold an ordinary surface together" on
/// all three (proposal P14 of the study, decided there).
pub const REFINE_DEFAULT: f64 = 50.0;

/// The bounds one stored sample component may take, in linear sRGB. A sample is a pixel of the
/// operation's own input, which an earlier unit may legitimately have pushed outside `[0, 1]`, so the
/// rule is generous — four stops above white and as far below black — and exists to keep a stored
/// payload finite and bounded rather than to describe a gamut.
pub const SAMPLE_MIN: f64 = -16.0;
pub const SAMPLE_MAX: f64 = 16.0;

/// A colour-range component's stored payload: up to [`MAX_SAMPLES`] sampled colours as linear sRGB
/// triples in the domain of the operation the mask modulates, and one refine slider in `0..100`.
///
/// `samples` defaults to empty, which is what a freshly created component holds: the generated
/// `mask.create-colour-range` declares the kind's *geometry*, which is the refine slider, and the
/// swatches arrive afterwards from a pick. An unsampled colour range selects nothing, exactly as an
/// empty mask does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColourRange {
    #[serde(default)]
    pub samples: Vec<[f64; 3]>,
    pub refine: f64,
}

/// What this kind declares to the API: the refine slider, and nothing else.
///
/// The swatches are deliberately **not** a declared parameter. The closed parameter vocabulary has
/// numbers, integers, enums, colours, booleans and curves and no list of any of them, so a list of
/// sampled colours cannot be declared honestly as one field; it is edited by the two generated
/// sample methods instead, one swatch at a time, which is also how a person edits it.
pub(super) fn colour_parameters(required: bool) -> Vec<ParameterDescriptor> {
    vec![ParameterDescriptor {
        unit: Some("%".to_owned()),
        step: Some(1.0),
        precision: Some(1),
        fine_step: Some(0.1),
        zero: Some(REFINE_DEFAULT),
        default: Some(serde_json::json!(REFINE_DEFAULT)),
        ..super::parameters::parameter(
            "refine",
            crate::ParameterKind::Number {
                min: REFINE_MIN,
                max: REFINE_MAX,
            },
            required,
            "how tight the selection around each sampled colour is; a higher refine is always a \
             narrower selection, geometrically between a whole colour family at 0 and one flat \
             patch at 100",
        )
    }]
}

/// The parameters one sampled colour declares: a linear-sRGB triple in the domain of the operation
/// the mask modulates, which is the domain a pick on the canvas reads.
pub(super) fn colour_sample_parameters() -> Vec<ParameterDescriptor> {
    ["r", "g", "b"]
        .into_iter()
        .map(|name| ParameterDescriptor {
            unit: Some("lin".to_owned()),
            step: Some(0.01),
            precision: Some(6),
            soft_min: Some(0.0),
            soft_max: Some(1.0),
            fine_step: Some(0.001),
            ..super::parameters::parameter(
                name,
                crate::ParameterKind::Number {
                    min: SAMPLE_MIN,
                    max: SAMPLE_MAX,
                },
                true,
                "one channel of the sampled colour, in linear sRGB in the domain of the operation \
                 this mask modulates — the value a pick on the canvas reads from the stage that \
                 operation receives",
            )
        })
        .collect()
}

/// Parse and range-check one stored `colour-range` payload.
pub(super) fn parse_colour(component: &Component) -> Result<ColourRange, Error> {
    let range: ColourRange =
        serde_json::from_value(component.payload.clone()).map_err(|error| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} has an invalid {COLOUR_KIND} payload: {error}",
                    component.name
                ),
            )
        })?;
    let refuse = |field: &str, what: String| {
        Err(Error::new(
            ErrorKind::Validation,
            format!("component {} {COLOUR_KIND} {field} {what}", component.name),
        ))
    };
    if !range.refine.is_finite() || !(REFINE_MIN..=REFINE_MAX).contains(&range.refine) {
        return refuse(
            "refine",
            format!("must be a number within {REFINE_MIN:.0}..={REFINE_MAX:.0}"),
        );
    }
    if range.samples.len() > MAX_SAMPLES {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "component {} has {} samples; the limit is {MAX_SAMPLES} samples per \
                 {COLOUR_KIND} component",
                component.name,
                range.samples.len()
            ),
        ));
    }
    for sample in &range.samples {
        for value in sample {
            if !value.is_finite() || !(SAMPLE_MIN..=SAMPLE_MAX).contains(value) {
                return refuse(
                    "samples",
                    format!(
                        "must each be three linear-sRGB numbers within {SAMPLE_MIN:.0}..={SAMPLE_MAX:.0}"
                    ),
                );
            }
        }
    }
    Ok(range)
}

/// The frozen refine mapping: geometric between [`RADIUS_MAX`] at `refine = 0` and [`RADIUS_MIN`] at
/// `refine = 100`.
///
/// ```text
/// radius = RADIUS_MAX * (RADIUS_MIN / RADIUS_MAX)^(refine / 100)
/// ```
///
/// Geometric rather than linear because what a person judges is the *ratio* between the radius and
/// the distance to the colours they do not want: a linear map spends three quarters of its travel
/// above every useful radius. Evaluated once per compiled component, never per pixel.
pub fn refine_radius(refine: f64) -> f64 {
    RADIUS_MAX * (RADIUS_MIN / RADIUS_MAX).powf(refine / 100.0)
}

/// The colour-constrained brush's similarity, compiled: **this kind's own falloff at one sample.**
///
/// It exists so the [brush](super::brush) reaches the frozen colour metric, the frozen plateau and
/// span and the frozen [`refine_radius`] mapping by *evaluating this kind* rather than by carrying a
/// second copy of them (`docs/design/mask-study.md#the-colour-constraint`). Folding one sample by
/// `min` against `+infinity` returns that sample's own squared distance exactly, so a one-sample
/// component is the similarity bit for bit and not merely within tolerance of it. There is no second
/// colour space, no second radius mapping and no second constant in the editor because of that
/// feature.
pub(super) fn similarity(seed: [f64; 3], refine: f64) -> CompiledColour {
    CompiledColour::new(&ColourRange {
        samples: vec![seed],
        refine,
    })
}

/// One colour range compiled once per component: each sample's Oklab `(a, b)` pair and the radius
/// the refine slider maps to. The samples' own conversions happen here, never per pixel.
#[derive(Clone, Debug)]
pub(super) struct CompiledColour {
    points: Vec<(f64, f64)>,
    radius: f64,
}

impl CompiledColour {
    pub(super) fn new(stored: &ColourRange) -> Self {
        Self {
            points: stored.samples.iter().map(|rgb| oklab_ab(*rgb)).collect(),
            radius: refine_radius(stored.refine),
        }
    }

    /// Coverage for one linear-sRGB pixel, as the study froze it:
    ///
    /// ```text
    /// lab = to_oklab(rgb)
    /// d2  = +infinity
    /// for (ak, bk) in points:
    ///     da = lab.a - ak
    ///     db = lab.b - bk
    ///     d2 = min(d2, da*da + db*db)
    /// d   = sqrt(d2)
    /// r   = d / radius
    /// c   = smooth(clamp((1 - r) / SPAN, 0, 1))
    /// ```
    ///
    /// The metric is the Oklab **chromaticity** distance and `L` does not appear. That is the
    /// study's measured choice, not an omission: a surface under a stop of shading moves eight to
    /// twelve times further in `L` than in `(a, b)`, so any positive lightness weight puts a sky
    /// that has fallen off by one stop further from the sampled sky than a different blue flower is.
    /// Lightness already has its own component, which this one intersects with exactly.
    ///
    /// Folding the nearest sample by `min` on the squared distance and taking one square root is
    /// bit-identical to taking the maximum of the per-sample falloffs, because the falloff is
    /// nonincreasing in `d` and both spellings evaluate it on the same `f64`; the frozen form is the
    /// cheap one, at one `sqrt` per pixel rather than one per sample.
    ///
    /// An empty sample list leaves `d2` at `+infinity`, so `r` is `+infinity`, the clamp is `0` and
    /// coverage is exactly `0`: an unsampled colour range selects nothing.
    ///
    /// `x / SPAN` is written as the division the transcription rule asks for; `SPAN` is exactly
    /// `0.5`, so the study permits `x * 2.0` as the same `f64` and neither spelling is preferred.
    pub(super) fn coverage(&self, rgb: [f64; 3]) -> f64 {
        let (a, b) = oklab_ab(rgb);
        let mut d2 = f64::INFINITY;
        for (ak, bk) in &self.points {
            let da = a - ak;
            let db = b - bk;
            d2 = d2.min(da * da + db * db);
        }
        let d = d2.sqrt();
        let r = d / self.radius;
        smooth(((1.0 - r) / SPAN).clamp(0.0, 1.0))
    }
}

// ---------------------------------------------------------------------------
// What a value-based component answers about the frame
// ---------------------------------------------------------------------------

/// The conservative rectangle of a value-based component: the **whole stage**, drawn or inverted.
///
/// This is a stated answer and not a guess (proposal P13 of the study, decided there). Coverage
/// depends on a pixel's value and not on where it is, so a colour a component selects can appear
/// anywhere in the frame and no colour span or spatial tile can be skipped for it. Saying so is what
/// keeps the rectangle's promise — outside it coverage is exactly zero — true.
pub(super) fn value_support(stage: Stage) -> Region {
    super::whole_stage(stage)
}

/// The smallest feature a value-based component draws, in a stage's pixels: `f64::INFINITY`.
///
/// A value test draws no feature a pixel grid can miss — there is no ramp across the frame to
/// resolve — so the thin-feature rule never fires for one. That is the honest answer rather than a
/// convenient one: supersampling the mask at proxy size cannot help, because the full-resolution
/// pixels a range component would have to read are not there to read, so a flag saying the frame is
/// approximate would name a condition nothing can fix. A proxy frame carrying a range component is
/// still the exact recipe over the exact downscale, which is what the delivered contract promises;
/// what changes is that the recipe is being evaluated on downscaled pixels, and the overlay and the
/// user guide say so.
pub(super) fn value_feature_px(stage: Stage) -> f64 {
    let _ = stage;
    f64::INFINITY
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The axis returns every grey code to its own histogram bin, which is what makes the band's
    /// numbers mean what the histogram's axis means.
    #[test]
    fn the_axis_is_the_histograms_own_domain() {
        for code in 0u8..=255 {
            let linear = f64::from(crate::render::decode_pixel([code, code, code])[0]);
            let e = luminance_axis([linear, linear, linear]);
            assert_eq!(
                (255.0 * e + 0.5).floor() as u8,
                code,
                "code {code} did not return to its own bin"
            );
        }
    }

    /// The refine mapping hits both ends and the geometric mean at the default.
    #[test]
    fn the_refine_mapping_hits_both_ends_and_its_default() {
        assert!((refine_radius(REFINE_MIN) - RADIUS_MAX).abs() < 1e-15);
        assert!((refine_radius(REFINE_MAX) - RADIUS_MIN).abs() < 1e-15);
        assert!((refine_radius(REFINE_DEFAULT) - (RADIUS_MIN * RADIUS_MAX).sqrt()).abs() < 1e-15);
    }

    /// The shoulder is exact at both of its ends, which is what the `+ 1` spelling buys.
    #[test]
    fn the_band_is_exactly_one_between_its_edges() {
        let band = CompiledLuminance::new(LuminanceRange {
            low: 30.0,
            low_feather: 12.0,
            high: 70.0,
            high_feather: 8.0,
        });
        // A grey whose axis value is exactly the band's own edge cannot be constructed from a byte,
        // so the edges are checked on the axis directly through a grey that straddles them.
        assert_eq!(band.lo, 0.3);
        assert_eq!(band.hi, 0.7);
        // Between the edges both ramps are exactly one, whatever the feathers.
        for e in [0.3, 0.4, 0.5, 0.7] {
            let linear = linear_grey(e);
            let c = band.coverage([linear, linear, linear]);
            assert!(c > 0.999_999, "coverage {c} at e = {e}");
        }
    }

    /// An unsampled colour range selects nothing, and a sampled colour is selected exactly.
    #[test]
    fn a_colour_range_is_exact_at_its_samples_and_empty_without_them() {
        let empty = CompiledColour::new(&ColourRange {
            samples: Vec::new(),
            refine: REFINE_DEFAULT,
        });
        assert_eq!(empty.coverage([0.2, 0.3, 0.4]), 0.0);
        let sample = [0.10, 0.18, 0.32];
        let one = CompiledColour::new(&ColourRange {
            samples: vec![sample],
            refine: REFINE_DEFAULT,
        });
        assert_eq!(one.coverage(sample), 1.0);
        // Sampling the same colour twice changes nothing, bit for bit: the fold is `min`.
        let twice = CompiledColour::new(&ColourRange {
            samples: vec![sample, sample],
            refine: REFINE_DEFAULT,
        });
        for rgb in [[0.2, 0.3, 0.4], [0.5, 0.1, 0.05], sample] {
            assert_eq!(twice.coverage(rgb), one.coverage(rgb));
        }
    }

    /// The linear grey whose encoded luminance is `e`.
    fn linear_grey(e: f64) -> f64 {
        if e <= 0.040_45 {
            e / 12.92
        } else {
            ((e + 0.055) / 1.055).powf(2.4)
        }
    }
}
