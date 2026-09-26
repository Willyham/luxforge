//! Independent f64 reference for the frozen range-selection mathematics
//! (TASK-022): the luminance band and the Oklab colour range.
//!
//! This module shares no code with production — there is no production range
//! code yet, and when there is, this file is the oracle it cannot influence,
//! matching the convention `crates/luxforge-reference/src/mask.rs` and
//! `crates/luxforge-reference/src/vignette.rs` set. The frozen equations, every constant's
//! justification and every measured figure are written out in full in
//! `docs/design/range-study.md`; this file is their literal transcription, in
//! the same order and the same spelling, and the two must be read together. A
//! production unit that writes the same expressions in the same order is
//! bit-identical to this reference rather than merely within tolerance of it.
//!
//! Three deliberate reuses, so the editor keeps one of each rather than two
//! that differ for no reason a person could name:
//!
//! * the falloff is [`super::mask::smooth`], the shape the vignette froze and
//!   the mask study reuses;
//! * the luminance axis is [`super::tone::luminance`] composed with
//!   [`super::tone::encode_srgb_extended`], the delivered Basic layer's own
//!   Rec. 709 luminance and its analytically continued sRGB OETF;
//! * the colour space is [`super::colour`]'s Oklab, accepted by
//!   `docs/design/basic-colour.md` and reused unchanged by the mixer study.
//!
//! Domain: every function takes and returns plain `f64` and validates nothing.
//! Legality of a stored payload is [`feather_is_legal`] and [`refine_is_legal`],
//! which state the frozen rules but are not applied automatically; production
//! rejects an illegal payload before any of this is reached. Given finite
//! inputs that satisfy those rules, every step below is finite: the only
//! divisions are by a feather the rules bound away from zero (with an explicit
//! hard-edge branch for a feather that is exactly zero) and by a radius the
//! refine mapping bounds below by [`RADIUS_MIN`].
//!
//! Unlike the geometric components, a range component reads the **pixel value**
//! the operation it modulates receives, not the pixel's position. Every
//! consequence of that — the compiled-mask signature, the bounds rectangle, the
//! proxy phase and what a reordered layer selects — is in the study.

use super::colour::{self, Oklab};
use super::mask::smooth;
use super::tone::{encode_srgb_extended, luminance};

// ---------------------------------------------------------------------------
// The luminance band
// ---------------------------------------------------------------------------

/// The luminance axis: Rec. 709 relative luminance of the linear-sRGB input,
/// encoded through the analytically continued sRGB OETF. This is the domain the
/// histogram bins — the histogram's horizontal axis *is* the encoded output
/// value, quantized to a byte — so a number on the slider means what a person
/// reads off the histogram.
///
/// The result is **not clamped**: a linear luminance outside `[0, 1]`, which an
/// earlier unit may legitimately have produced, encodes monotonically to a
/// value outside `[0, 1]` and is treated as darker than black or brighter than
/// white rather than folded back onto the axis.
pub fn luminance_axis(rgb_linear: [f64; 3]) -> f64 {
    encode_srgb_extended(luminance(rgb_linear))
}

/// The two rejected luminance axes, kept only so the study's comparison is
/// reproducible. No production unit transcribes either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// The frozen one: Rec. 709 luminance on linear light, then encoded.
    LinearLuminance,
    /// Rec. 709 weights applied to the already-encoded channels — the "luma"
    /// a histogram of encoded values would suggest.
    EncodedLuma,
    /// `max(r, g, b)` encoded: the axis the clipping predicate uses.
    MaxChannel,
}

/// Evaluate one candidate luminance axis on a linear-sRGB triple.
pub fn axis_value(axis: Axis, rgb_linear: [f64; 3]) -> f64 {
    match axis {
        Axis::LinearLuminance => luminance_axis(rgb_linear),
        Axis::EncodedLuma => {
            let e = [
                encode_srgb_extended(rgb_linear[0]),
                encode_srgb_extended(rgb_linear[1]),
                encode_srgb_extended(rgb_linear[2]),
            ];
            0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]
        }
        Axis::MaxChannel => {
            let m = rgb_linear[0].max(rgb_linear[1]).max(rgb_linear[2]);
            encode_srgb_extended(m)
        }
    }
}

/// The smallest legal **non-zero** shoulder width, in slider units (the axis
/// times 100, so one unit is `1/100` of the encoded range and 2.55 output
/// codes). A feather of exactly `0` is the explicit hard-edge branch; anything
/// between is refused, because the shoulder's slope is `1.5 / feather` and a
/// shoulder narrower than one slider step is a hard edge asked for in a way the
/// arithmetic cannot bound. The floor bounds every reciprocal the band takes by
/// `100`.
pub const FEATHER_MIN: f64 = 1.0;

/// The largest legal shoulder width, in slider units: the whole axis.
pub const FEATHER_MAX: f64 = 100.0;

/// The frozen legality rule for a shoulder: finite, and either exactly zero
/// (the hard edge) or within `[FEATHER_MIN, FEATHER_MAX]`.
pub fn feather_is_legal(feather: f64) -> bool {
    feather.is_finite() && (feather == 0.0 || (FEATHER_MIN..=FEATHER_MAX).contains(&feather))
}

/// A luminance-range component's payload. `low` and `high` are on the slider's
/// `0..100` axis (the encoded axis times 100) with `low <= high`; the two
/// feathers are shoulder widths on the same axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LuminanceRange {
    pub low: f64,
    pub low_feather: f64,
    pub high: f64,
    pub high_feather: f64,
}

/// The payload's ordering rule. The mathematics is total either way — the band
/// simply selects nothing — but production rejects a crossed band rather than
/// rendering an empty selection a person cannot see the cause of.
pub fn luminance_range_is_legal(range: &LuminanceRange) -> bool {
    range.low.is_finite()
        && range.high.is_finite()
        && (0.0..=100.0).contains(&range.low)
        && (0.0..=100.0).contains(&range.high)
        && range.low <= range.high
        && feather_is_legal(range.low_feather)
        && feather_is_legal(range.high_feather)
}

/// A luminance band compiled once per component: the payload divided onto the
/// encoded axis, which is the only per-component work the band needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompiledLuminanceRange {
    pub lo: f64,
    pub hi: f64,
    pub lo_feather: f64,
    pub hi_feather: f64,
}

/// Compile a luminance payload: `/ 100` onto the encoded axis, once, so the
/// per-pixel path never multiplies the pixel's own value by 100.
pub fn compile_luminance_range(range: &LuminanceRange) -> CompiledLuminanceRange {
    CompiledLuminanceRange {
        lo: range.low / 100.0,
        hi: range.high / 100.0,
        lo_feather: range.low_feather / 100.0,
        hi_feather: range.high_feather / 100.0,
    }
}

/// The luminance band's coverage at an already-computed axis value `e`, in the
/// exact order a production unit transcribes:
///
/// ```text
/// rise = if lo_feather == 0 { if e >= lo { 1 } else { 0 } }
///        else { smooth(clamp((e - lo) / lo_feather + 1, 0, 1)) }
/// fall = if hi_feather == 0 { if e <= hi { 1 } else { 0 } }
///        else { smooth(clamp((hi - e) / hi_feather + 1, 0, 1)) }
/// c    = min(rise, fall)
/// ```
///
/// The `+ 1` spelling is frozen rather than the equivalent
/// `(e - (lo - lo_feather)) / lo_feather`, because it is exact at both ends of
/// the shoulder: at `e == lo` the numerator is exactly `0`, so the ratio is
/// exactly `1`; at `e == lo - lo_feather` the ratio is exactly `-1 + 1 == 0`.
/// The other spelling rounds `lo - lo_feather` first and can land a few ulps
/// either side of both.
///
/// `min` is the frozen composition algebra's own intersection, which is what
/// the band is: darker than the high edge **and** brighter than the low edge.
/// It introduces no rounding at all.
///
/// The hard branch follows the radial's `span == 0` discipline: a feather of
/// exactly zero, and only that, takes it, so no division by a vanishing
/// shoulder is ever evaluated.
pub fn luminance_coverage_at(band: &CompiledLuminanceRange, e: f64) -> f64 {
    let rise = if band.lo_feather == 0.0 {
        if e >= band.lo { 1.0 } else { 0.0 }
    } else {
        smooth((((e - band.lo) / band.lo_feather) + 1.0).clamp(0.0, 1.0))
    };
    let fall = if band.hi_feather == 0.0 {
        if e <= band.hi { 1.0 } else { 0.0 }
    } else {
        smooth((((band.hi - e) / band.hi_feather) + 1.0).clamp(0.0, 1.0))
    };
    rise.min(fall)
}

/// The luminance band's coverage for one linear-sRGB pixel: the axis, then the
/// band.
pub fn luminance_coverage(band: &CompiledLuminanceRange, rgb_linear: [f64; 3]) -> f64 {
    luminance_coverage_at(band, luminance_axis(rgb_linear))
}

/// The design's literal four-branch spelling of one shoulder, kept only so the
/// tests can prove it equals the clamped one-branch form above. No production
/// unit transcribes this one.
pub fn luminance_coverage_branch_form(band: &CompiledLuminanceRange, e: f64) -> f64 {
    let lo_edge = band.lo - band.lo_feather;
    let hi_edge = band.hi + band.hi_feather;
    let rise = if band.lo_feather == 0.0 {
        if e >= band.lo { 1.0 } else { 0.0 }
    } else if e <= lo_edge {
        0.0
    } else if e >= band.lo {
        1.0
    } else {
        smooth(((e - band.lo) / band.lo_feather) + 1.0)
    };
    let fall = if band.hi_feather == 0.0 {
        if e <= band.hi { 1.0 } else { 0.0 }
    } else if e >= hi_edge {
        0.0
    } else if e <= band.hi {
        1.0
    } else {
        smooth(((band.hi - e) / band.hi_feather) + 1.0)
    };
    rise.min(fall)
}

// ---------------------------------------------------------------------------
// The colour range
// ---------------------------------------------------------------------------

/// The largest number of sampled colours one colour-range component holds. This
/// is a product bound (the panel shows five swatches), not a numerical one:
/// the fold below is `min`, which is exact and associative, so nothing in the
/// mathematics changes if it is raised.
pub const MAX_SAMPLES: usize = 5;

/// The loosest selection radius, in Oklab `(a, b)` units, at `refine = 0`.
/// `0.25` is 41% of the largest chromaticity distance between two in-gamut
/// sRGB colours (`0.6165`, between the blue and yellow primaries) and 78% of
/// the largest Oklab chroma on the gamut surface (`0.3225`, at sRGB magenta),
/// so the loosest setting selects a broad family of related colours and still
/// leaves the opposite side of the wheel out.
pub const RADIUS_MAX: f64 = 0.25;

/// The tightest selection radius, in Oklab `(a, b)` units, at `refine = 100`.
/// `0.005` is below the measured chromaticity spread of a single surface under
/// half a stop of shading, so the tightest setting selects one flat patch and
/// little else. It also bounds the per-pixel reciprocal by `200`.
pub const RADIUS_MIN: f64 = 0.005;

/// The fraction of the radius that is fully selected: coverage is exactly `1`
/// out to `PLATEAU * radius` and falls off over the rest. Sized so that at the
/// default refine the plateau covers a single surface's own chromaticity spread
/// under a stop of shading; see the study.
pub const PLATEAU: f64 = 0.5;

/// The falloff span, `1 - PLATEAU`. It is exactly `0.5`, so dividing by it is
/// exact and the transcription rule's ban on precomputed reciprocals costs
/// nothing here: `x / SPAN` and `x * 2.0` are the same `f64`.
pub const SPAN: f64 = 1.0 - PLATEAU;

/// The frozen legality rule for the refine slider.
pub fn refine_is_legal(refine: f64) -> bool {
    refine.is_finite() && (0.0..=100.0).contains(&refine)
}

/// The frozen refine mapping: geometric between [`RADIUS_MAX`] at `refine = 0`
/// and [`RADIUS_MIN`] at `refine = 100`.
///
/// ```text
/// radius = RADIUS_MAX * (RADIUS_MIN / RADIUS_MAX)^(refine / 100)
/// ```
///
/// Geometric rather than linear because what a person judges is the *ratio*
/// between the radius and the distance to the colours they do not want: a
/// linear map spends three quarters of its travel above every useful radius.
/// Evaluated once per compiled component, never per pixel.
pub fn refine_radius(refine: f64) -> f64 {
    RADIUS_MAX * (RADIUS_MIN / RADIUS_MAX).powf(refine / 100.0)
}

/// The rejected linear refine mapping, kept only for the study's comparison.
pub fn refine_radius_linear(refine: f64) -> f64 {
    RADIUS_MAX + (RADIUS_MIN - RADIUS_MAX) * (refine / 100.0)
}

/// A colour-range component's payload: up to [`MAX_SAMPLES`] sampled colours as
/// **linear sRGB** triples in the domain of the operation the mask modulates,
/// and one refine slider in `0..100`.
#[derive(Clone, Debug, PartialEq)]
pub struct ColourRange {
    pub samples: Vec<[f64; 3]>,
    pub refine: f64,
}

/// A colour range compiled once per component: each sample's Oklab `(a, b)`
/// pair and the radius the refine slider maps to. The samples' own Oklab
/// conversions happen here, never per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledColourRange {
    pub points: Vec<(f64, f64)>,
    pub radius: f64,
}

/// Compile a colour-range payload.
pub fn compile_colour_range(range: &ColourRange) -> CompiledColourRange {
    CompiledColourRange {
        points: range
            .samples
            .iter()
            .map(|rgb| {
                let lab = colour::to_oklab(*rgb);
                (lab.a, lab.b)
            })
            .collect(),
        radius: refine_radius(range.refine),
    }
}

/// The colour range's coverage for one linear-sRGB pixel, in the exact order a
/// production unit transcribes:
///
/// ```text
/// lab = to_oklab(rgb)                       -- reference/colour.rs, unchanged
/// d2  = +infinity
/// for (ak, bk) in points:                   -- at most MAX_SAMPLES of them
///     da = lab.a - ak
///     db = lab.b - bk
///     d2 = min(d2, da*da + db*db)
/// d   = sqrt(d2)
/// r   = d / radius                          -- radius compiled from refine
/// c   = smooth(clamp((1 - r) / SPAN, 0, 1))
/// ```
///
/// The metric is the Oklab **chromaticity** distance: `L` does not appear. That
/// is the study's measured choice, not an omission — a surface under a stop of
/// shading moves eight to twelve times further in `L` than in `(a, b)`, and lightness
/// already has its own component, which this one intersects with exactly.
///
/// Folding the nearest sample by `min` on the squared distance and taking one
/// square root is **bit-identical** to taking the maximum of the per-sample
/// falloffs, because the falloff is nonincreasing in `d` and both spellings
/// evaluate it on the same `f64`; the tests assert that rather than assuming
/// it. It costs one `sqrt` per pixel instead of one per sample.
///
/// An empty sample list leaves `d2` at `+infinity`, so `r` is `+infinity`,
/// the clamp is `0` and coverage is exactly `0`: an unsampled colour range
/// selects nothing, exactly as an empty mask does.
pub fn colour_coverage(range: &CompiledColourRange, rgb_linear: [f64; 3]) -> f64 {
    let lab = colour::to_oklab(rgb_linear);
    colour_coverage_at(range, lab)
}

/// The colour range's coverage for an already-converted Oklab colour.
pub fn colour_coverage_at(range: &CompiledColourRange, lab: Oklab) -> f64 {
    let mut d2 = f64::INFINITY;
    for (ak, bk) in &range.points {
        let da = lab.a - ak;
        let db = lab.b - bk;
        d2 = d2.min(da * da + db * db);
    }
    let d = d2.sqrt();
    let r = d / range.radius;
    smooth(((1.0 - r) / SPAN).clamp(0.0, 1.0))
}

/// The max-of-falloffs spelling, kept only so the tests can prove it equals
/// [`colour_coverage_at`] bit for bit. No production unit transcribes this one:
/// it evaluates one `sqrt` per sample.
pub fn colour_coverage_max_form(range: &CompiledColourRange, lab: Oklab) -> f64 {
    let mut c = 0.0f64;
    for (ak, bk) in &range.points {
        let da = lab.a - ak;
        let db = lab.b - bk;
        let d = (da * da + db * db).sqrt();
        let r = d / range.radius;
        c = c.max(smooth(((1.0 - r) / SPAN).clamp(0.0, 1.0)));
    }
    c
}

/// The probabilistic-sum spelling of the multi-sample combination, kept only
/// for the study's comparison against the frozen `max`. It is the colour
/// range's own copy of the product algebra the mask study rejected, and it
/// fails the same way: adding the same sample twice changes the coverage.
pub fn colour_coverage_product_form(range: &CompiledColourRange, lab: Oklab) -> f64 {
    let mut c = 0.0f64;
    for (ak, bk) in &range.points {
        let da = lab.a - ak;
        let db = lab.b - bk;
        let d = (da * da + db * db).sqrt();
        let r = d / range.radius;
        let ck = smooth(((1.0 - r) / SPAN).clamp(0.0, 1.0));
        c = 1.0 - (1.0 - c) * (1.0 - ck);
    }
    c
}

/// The three candidate colour metrics the study compares. The frozen one is
/// [`Metric::Chromaticity`]; the other two exist so the comparison is
/// reproducible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Metric {
    /// The frozen one: `sqrt(da² + db²)`, no lightness term.
    Chromaticity,
    /// `sqrt((w·dL)² + da² + db²)` with the stated lightness weight.
    Weighted(f64),
}

/// Distance between two Oklab colours under one candidate metric.
pub fn oklab_distance(metric: Metric, p: Oklab, q: Oklab) -> f64 {
    let da = p.a - q.a;
    let db = p.b - q.b;
    match metric {
        Metric::Chromaticity => (da * da + db * db).sqrt(),
        Metric::Weighted(w) => {
            let dl = w * (p.l - q.l);
            (dl * dl + da * da + db * db).sqrt()
        }
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn the_axis_is_the_histograms_own_domain() {
        // A mid-grey byte decodes, weights to the same linear value on all three
        // channels, and encodes back to exactly the encoded value the byte came
        // from: the axis and the histogram bin the same number.
        for code in [0u8, 1, 64, 128, 200, 255] {
            let linear = super::super::srgb_to_linear(code);
            let e = luminance_axis([linear, linear, linear]);
            let quantized = (255.0 * e + 0.5).floor() as u8;
            assert_eq!(quantized, code, "code {code} did not return to its own bin");
        }
    }

    #[test]
    fn the_shoulder_is_exact_at_both_of_its_ends() {
        let band = compile_luminance_range(&LuminanceRange {
            low: 30.0,
            low_feather: 12.0,
            high: 70.0,
            high_feather: 8.0,
        });
        // Exactly 1 at both band edges: this is the property the `+ 1`
        // spelling buys, and it is exact because the numerator is exactly 0.
        assert_eq!(luminance_coverage_at(&band, band.lo), 1.0);
        assert_eq!(luminance_coverage_at(&band, band.hi), 1.0);
        // At the shoulder's outer end the caller's own `lo - lo_feather` is a
        // rounded value, so coverage there is not exactly zero but is below
        // 1e-28; it is exactly zero for anything past it.
        assert!(luminance_coverage_at(&band, band.lo - band.lo_feather) < 1e-28);
        assert!(luminance_coverage_at(&band, band.hi + band.hi_feather) < 1e-28);
        assert_eq!(
            luminance_coverage_at(&band, band.lo - band.lo_feather - 1e-12),
            0.0
        );
        assert_eq!(
            luminance_coverage_at(&band, band.hi + band.hi_feather + 1e-12),
            0.0
        );
    }

    #[test]
    fn a_zero_feather_is_the_hard_edge_branch() {
        let band = compile_luminance_range(&LuminanceRange {
            low: 40.0,
            low_feather: 0.0,
            high: 60.0,
            high_feather: 0.0,
        });
        assert_eq!(luminance_coverage_at(&band, 0.399), 0.0);
        assert_eq!(luminance_coverage_at(&band, 0.4), 1.0);
        assert_eq!(luminance_coverage_at(&band, 0.6), 1.0);
        assert_eq!(luminance_coverage_at(&band, 0.601), 0.0);
    }

    #[test]
    fn the_refine_mapping_hits_both_ends_and_the_documented_default() {
        assert!((refine_radius(0.0) - RADIUS_MAX).abs() < 1e-15);
        assert!((refine_radius(100.0) - RADIUS_MIN).abs() < 1e-15);
        // refine = 50 is the geometric mean of the two ends.
        let mid = (RADIUS_MIN * RADIUS_MAX).sqrt();
        assert!((refine_radius(50.0) - mid).abs() < 1e-15);
    }

    #[test]
    fn an_unsampled_colour_range_selects_nothing() {
        let compiled = compile_colour_range(&ColourRange {
            samples: Vec::new(),
            refine: 50.0,
        });
        let c = colour_coverage(&compiled, [0.2, 0.3, 0.4]);
        assert_eq!(c, 0.0);
    }

    #[test]
    fn a_sampled_colour_is_selected_exactly() {
        let sample = [0.10, 0.18, 0.32];
        let compiled = compile_colour_range(&ColourRange {
            samples: vec![sample],
            refine: 50.0,
        });
        assert_eq!(colour_coverage(&compiled, sample), 1.0);
    }

    #[test]
    fn feather_legality_rejects_the_gap_below_the_floor() {
        assert!(feather_is_legal(0.0));
        assert!(feather_is_legal(FEATHER_MIN));
        assert!(feather_is_legal(FEATHER_MAX));
        assert!(!feather_is_legal(0.5));
        assert!(!feather_is_legal(-1.0));
        assert!(!feather_is_legal(FEATHER_MAX * 2.0));
        assert!(!feather_is_legal(f64::NAN));
        assert!(!feather_is_legal(f64::INFINITY));
    }
}
