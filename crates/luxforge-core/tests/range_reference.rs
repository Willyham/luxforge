//! TASK-022: independent proofs for the frozen range-selection mathematics —
//! the luminance band's axis, band and shoulders, and the Oklab colour range's
//! metric, multi-sample combination and refine mapping.
//!
//! This binary shares no code with `luxforge-core`'s production sources. The
//! frozen equations live in `tests/reference/range.rs`; the mathematics, the
//! rejected alternatives, the honest limits and every measured figure quoted
//! below are written out in full in `docs/design/range-study.md`, which this
//! file's test names track.
//!
//! The study's dense figures are printed by the one ignored test at the end:
//!
//! ```sh
//! cargo test --release --locked --package luxforge-core --test range_reference \
//!     -- --ignored --nocapture range_study_figures
//! ```

mod reference;

use reference::colour::{self, Oklab};
use reference::mask::smooth;
use reference::range::{
    Axis, ColourRange, CompiledColourRange, CompiledLuminanceRange, FEATHER_MAX, FEATHER_MIN,
    LuminanceRange, MAX_SAMPLES, Metric, PLATEAU, RADIUS_MAX, RADIUS_MIN, SPAN, axis_value,
    colour_coverage, colour_coverage_at, colour_coverage_max_form, colour_coverage_product_form,
    compile_colour_range, compile_luminance_range, feather_is_legal, luminance_axis,
    luminance_coverage, luminance_coverage_at, luminance_coverage_branch_form,
    luminance_range_is_legal, oklab_distance, refine_is_legal, refine_radius, refine_radius_linear,
};
use reference::{linear_to_code, srgb_to_linear};

// ---------------------------------------------------------------------------
// Test-only helpers, kept local to this file per the reference tree's
// convention (see `mask_reference.rs` and `vignette_reference.rs`).
// ---------------------------------------------------------------------------

/// SplitMix64: dependency-free and fully reproducible, so every fixed-seed
/// figure below is the same on any machine and any Rust version.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + u * (hi - lo)
    }

    fn next_usize(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    /// Two uniforms summed and centred: a bounded, reproducible stand-in for
    /// sensor noise, with no distribution claim beyond "symmetric about zero".
    fn next_noise(&mut self) -> f64 {
        self.next_range(-1.0, 1.0) + self.next_range(-1.0, 1.0)
    }
}

/// The widely published sRGB renderings of the 24-patch reflective colour
/// chart (BabelColor's averaged values). They are used here because they are
/// *measured surface colours* — skin, foliage, sky, neutrals — so the failure
/// cases below are about real photographic subjects rather than about numbers
/// chosen to make a point. No equivalence with any product's rendering of the
/// chart is claimed.
const CHART: [(&str, [u8; 3]); 24] = [
    ("dark skin", [115, 82, 68]),
    ("light skin", [194, 150, 130]),
    ("blue sky", [98, 122, 157]),
    ("foliage", [87, 108, 67]),
    ("blue flower", [133, 128, 177]),
    ("bluish green", [103, 189, 170]),
    ("orange", [214, 126, 44]),
    ("purplish blue", [80, 91, 166]),
    ("moderate red", [193, 90, 99]),
    ("purple", [94, 60, 108]),
    ("yellow green", [157, 188, 64]),
    ("orange yellow", [224, 163, 46]),
    ("blue", [56, 61, 150]),
    ("green", [70, 148, 73]),
    ("red", [175, 54, 60]),
    ("yellow", [231, 199, 31]),
    ("magenta", [187, 86, 149]),
    ("cyan", [8, 133, 161]),
    ("white", [243, 243, 242]),
    ("neutral 8", [200, 200, 200]),
    ("neutral 6.5", [160, 160, 160]),
    ("neutral 5", [122, 122, 121]),
    ("neutral 3.5", [85, 85, 85]),
    ("black", [52, 52, 52]),
];

/// Three surfaces a photograph is full of, used for the scenes below.
fn patch(name: &str) -> [f64; 3] {
    let codes = CHART
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no chart patch named {name}"))
        .1;
    [
        srgb_to_linear(codes[0]),
        srgb_to_linear(codes[1]),
        srgb_to_linear(codes[2]),
    ]
}

/// Two surfaces that are not on the chart and are exactly the awkward ones:
/// bare oak flooring and a terracotta pot, both of which a person expects a
/// "select the skin" colour range to leave alone.
const WOOD: [u8; 3] = [160, 110, 70];
const TERRACOTTA: [u8; 3] = [180, 100, 70];

fn codes_to_linear(codes: [u8; 3]) -> [f64; 3] {
    [
        srgb_to_linear(codes[0]),
        srgb_to_linear(codes[1]),
        srgb_to_linear(codes[2]),
    ]
}

/// The same surface under a linear gain: what shading, a cloud shadow or a
/// fall-off across a sky does to a pixel. Chromaticity is unchanged by
/// construction, so any movement in a colour metric is the metric's own
/// lightness sensitivity.
fn shaded(rgb: [f64; 3], gain: f64) -> [f64; 3] {
    [rgb[0] * gain, rgb[1] * gain, rgb[2] * gain]
}

/// The band every "ordinary" claim below uses: a broad highlight selection with
/// soft shoulders.
fn study_band() -> CompiledLuminanceRange {
    compile_luminance_range(&LuminanceRange {
        low: 55.0,
        low_feather: 15.0,
        high: 92.0,
        high_feather: 8.0,
    })
}

/// The colour range every "ordinary" claim below uses: one sampled sky at the
/// default refine.
fn study_colour_range() -> CompiledColourRange {
    compile_colour_range(&ColourRange {
        samples: vec![patch("blue sky")],
        refine: 50.0,
    })
}

fn sample_band(rng: &mut SplitMix64) -> LuminanceRange {
    let low = rng.next_range(0.0, 100.0);
    let high = rng.next_range(low, 100.0);
    let feather = |rng: &mut SplitMix64| {
        if rng.next_usize(4) == 0 {
            0.0
        } else {
            rng.next_range(FEATHER_MIN, FEATHER_MAX)
        }
    };
    LuminanceRange {
        low,
        low_feather: feather(rng),
        high,
        high_feather: feather(rng),
    }
}

fn sample_colour_range(rng: &mut SplitMix64) -> ColourRange {
    let count = 1 + rng.next_usize(MAX_SAMPLES);
    let samples = (0..count)
        .map(|_| {
            [
                rng.next_range(0.0, 1.0),
                rng.next_range(0.0, 1.0),
                rng.next_range(0.0, 1.0),
            ]
        })
        .collect();
    ColourRange {
        samples,
        refine: rng.next_range(0.0, 100.0),
    }
}

/// The blend a masked colour operation performs, in output codes, so the
/// visibility claims are quoted in something a person can see. The effect is
/// `+1 EV` on the input, the study's standard yardstick.
fn code_after_masked_exposure(input_linear: f64, coverage: f64) -> u8 {
    let effect = input_linear * 2.0;
    linear_to_code((1.0 - coverage) * input_linear + coverage * effect)
}

// ---------------------------------------------------------------------------
// The luminance axis
// ---------------------------------------------------------------------------

#[test]
fn the_luminance_axis_is_the_domain_the_histogram_bins() {
    // For a neutral pixel the axis returns exactly the encoded value the output
    // byte quantizes, so the slider's number and the histogram's horizontal
    // position are the same number. Checked on every one of the 256 codes.
    for code in 0u8..=255 {
        let linear = srgb_to_linear(code);
        let e = luminance_axis([linear, linear, linear]);
        let quantized = (255.0 * e.clamp(0.0, 1.0) + 0.5).floor() as u8;
        assert_eq!(quantized, code, "grey code {code} did not bin to itself");
        assert_eq!(
            quantized,
            linear_to_code(linear),
            "grey code {code}: the axis and the output quantizer disagree"
        );
    }
}

#[test]
fn the_axis_is_monotone_and_finite_outside_the_gamut() {
    // An earlier unit may hand on a linear value below 0 or above 1. The axis
    // is the analytically continued OETF, so it stays finite and strictly
    // increasing across both boundaries rather than folding back.
    let mut previous = f64::NEG_INFINITY;
    let mut l = -0.5;
    while l <= 2.0 {
        let e = luminance_axis([l, l, l]);
        assert!(e.is_finite(), "luminance {l} gave a non-finite axis value");
        assert!(e > previous, "axis was not increasing at luminance {l}");
        previous = e;
        l += 1.0 / 512.0;
    }
}

#[test]
fn the_rejected_luminance_axes_differ_from_the_frozen_one_by_the_recorded_amounts() {
    // Two alternatives were implemented alongside the frozen axis: Rec. 709
    // weights applied to the already-encoded channels, and the maximum channel
    // (the axis the clipping predicate uses). Both are recorded with their
    // measured separation over a dense sweep of the sRGB cube.
    let mut worst_luma = 0.0f64;
    let mut worst_max = 0.0f64;
    let step = 8;
    let mut r = 0u16;
    while r <= 255 {
        let mut g = 0u16;
        while g <= 255 {
            let mut b = 0u16;
            while b <= 255 {
                let rgb = codes_to_linear([r as u8, g as u8, b as u8]);
                let frozen = axis_value(Axis::LinearLuminance, rgb);
                worst_luma = worst_luma.max((axis_value(Axis::EncodedLuma, rgb) - frozen).abs());
                worst_max = worst_max.max((axis_value(Axis::MaxChannel, rgb) - frozen).abs());
                b += step;
            }
            g += step;
        }
        r += step;
    }
    // Recorded in the study, to 1e-4 of the 0..1 axis.
    assert!(
        (worst_luma - 0.277_276).abs() < 1e-4,
        "encoded-luma separation moved: {worst_luma}"
    );
    assert!(
        (worst_max - 0.683_838).abs() < 1e-4,
        "max-channel separation moved: {worst_max}"
    );

    // The worked example the study quotes: a saturated red surface.
    let red = patch("red");
    let frozen = 100.0 * axis_value(Axis::LinearLuminance, red);
    let luma = 100.0 * axis_value(Axis::EncodedLuma, red);
    let max = 100.0 * axis_value(Axis::MaxChannel, red);
    assert!((frozen - 38.23).abs() < 0.01, "frozen red axis {frozen}");
    assert!((luma - 31.43).abs() < 0.01, "encoded-luma red axis {luma}");
    assert!((max - 68.63).abs() < 0.01, "max-channel red axis {max}");
}

// ---------------------------------------------------------------------------
// The luminance band
// ---------------------------------------------------------------------------

#[test]
fn the_clamped_band_form_equals_the_branch_form_bit_for_bit() {
    // The frozen spelling is one clamped branch per shoulder; the design's
    // literal four-branch spelling must be the same `f64`, not merely within
    // tolerance, because `smooth` returns exactly 0.0 and exactly 1.0 at the
    // clamp's ends.
    let mut rng = SplitMix64(0x5EED_0022);
    let mut compared = 0u32;
    for _ in 0..400 {
        let payload = sample_band(&mut rng);
        assert!(luminance_range_is_legal(&payload), "{payload:?}");
        let band = compile_luminance_range(&payload);
        for i in 0..=300 {
            let e = -0.2 + 1.4 * f64::from(i) / 300.0;
            let clamped = luminance_coverage_at(&band, e);
            let branch = luminance_coverage_branch_form(&band, e);
            assert_eq!(
                clamped.to_bits(),
                branch.to_bits(),
                "{payload:?} at e={e}: {clamped} vs {branch}"
            );
            compared += 1;
        }
    }
    assert!(compared > 100_000, "only {compared} points compared");
}

#[test]
fn the_band_is_exactly_one_inside_and_exactly_zero_outside() {
    let mut rng = SplitMix64(0x5EED_0023);
    for _ in 0..500 {
        let payload = sample_band(&mut rng);
        let band = compile_luminance_range(&payload);
        // Exactly 1 at both band edges and anywhere between them.
        assert_eq!(luminance_coverage_at(&band, band.lo), 1.0, "{payload:?}");
        assert_eq!(luminance_coverage_at(&band, band.hi), 1.0, "{payload:?}");
        let mid = 0.5 * (band.lo + band.hi);
        assert_eq!(luminance_coverage_at(&band, mid), 1.0, "{payload:?}");

        // Outside, on each side. A soft shoulder's outer end is a rounded
        // value, so coverage there is below 1e-28 rather than exactly zero, and
        // exactly zero for anything past it; a hard edge is exactly 1 at the
        // band edge and exactly 0 one ulp past it.
        let lo_end = band.lo - band.lo_feather;
        let hi_end = band.hi + band.hi_feather;
        if band.lo_feather == 0.0 {
            assert_eq!(luminance_coverage_at(&band, lo_end), 1.0, "{payload:?}");
        } else {
            assert!(luminance_coverage_at(&band, lo_end) < 1e-28, "{payload:?}");
        }
        if band.hi_feather == 0.0 {
            assert_eq!(luminance_coverage_at(&band, hi_end), 1.0, "{payload:?}");
        } else {
            assert!(luminance_coverage_at(&band, hi_end) < 1e-28, "{payload:?}");
        }
        assert_eq!(
            luminance_coverage_at(&band, lo_end - 1e-9),
            0.0,
            "{payload:?}"
        );
        assert_eq!(
            luminance_coverage_at(&band, hi_end + 1e-9),
            0.0,
            "{payload:?}"
        );
    }
}

#[test]
fn the_band_rises_then_falls_and_never_leaves_the_unit_interval() {
    // Coverage is nondecreasing up to the low edge and nonincreasing after the
    // high edge, so the band is one connected selection with no rings — the
    // luminance analogue of the radial's "no rings" property.
    let mut rng = SplitMix64(0x5EED_0024);
    for _ in 0..300 {
        let payload = sample_band(&mut rng);
        let band = compile_luminance_range(&payload);
        let mut previous = 0.0;
        for i in 0..=2000 {
            let e = band.lo - band.lo_feather
                + (band.lo - (band.lo - band.lo_feather)) * f64::from(i) / 2000.0;
            let c = luminance_coverage_at(&band, e);
            assert!((0.0..=1.0).contains(&c), "{payload:?} at {e}: {c}");
            assert!(c >= previous - 1e-15, "{payload:?} fell on the rise at {e}");
            previous = c;
        }
        let mut previous = 1.0;
        for i in 0..=2000 {
            let e = band.hi + band.hi_feather * f64::from(i) / 2000.0;
            let c = luminance_coverage_at(&band, e);
            assert!((0.0..=1.0).contains(&c), "{payload:?} at {e}: {c}");
            assert!(c <= previous + 1e-15, "{payload:?} rose on the fall at {e}");
            previous = c;
        }
    }
}

#[test]
fn a_zero_feather_is_a_hard_edge_and_no_vanishing_shoulder_is_divided_by() {
    // The hard branch is taken on an exact zero and only there. Unlike the
    // radial's feather, there is no second route to it by rounding, because the
    // legality floor refuses every width between 0 and FEATHER_MIN: the study
    // states that as the difference between the two components.
    let band = compile_luminance_range(&LuminanceRange {
        low: 40.0,
        low_feather: 0.0,
        high: 60.0,
        high_feather: 0.0,
    });
    for i in 0..=1000 {
        let e = f64::from(i) / 1000.0;
        let c = luminance_coverage_at(&band, e);
        assert!(c == 0.0 || c == 1.0, "hard band gave {c} at {e}");
    }
    assert_eq!(luminance_coverage_at(&band, 0.4), 1.0);
    assert_eq!(luminance_coverage_at(&band, 0.6), 1.0);
    assert!(luminance_coverage_at(&band, 0.4 - f64::EPSILON) == 0.0);

    // The smallest legal non-zero shoulder is the smooth branch, checked from
    // the other side so the boundary is not asserted from one side only.
    let soft = compile_luminance_range(&LuminanceRange {
        low: 40.0,
        low_feather: FEATHER_MIN,
        high: 60.0,
        high_feather: FEATHER_MIN,
    });
    let halfway = luminance_coverage_at(&soft, 0.395);
    assert!(
        halfway > 0.0 && halfway < 1.0,
        "the smallest legal shoulder was not smooth: {halfway}"
    );
    assert!(!feather_is_legal(FEATHER_MIN / 2.0));
    // A denormal-sized feather cannot reach the hard branch by rounding either:
    // dividing by 100 does not underflow to zero for anything the floor admits,
    // so the branch is reachable only by an exact zero.
    let smallest: f64 = f64::MIN_POSITIVE;
    assert_ne!(smallest / 100.0, 0.0);
}

#[test]
fn the_shoulder_slope_is_bounded_by_the_frozen_floor() {
    // The whole conditioning argument of the frozen tolerance rests on this:
    // the shoulder's slope in the axis's own units is at most `1.5 / feather`,
    // so the floor on a non-zero feather bounds it at 150 per unit of the
    // encoded axis, which is 0.588 of coverage per output code of input.
    for feather in [FEATHER_MIN, 2.0, 5.0, 15.0, 50.0, FEATHER_MAX] {
        let band = compile_luminance_range(&LuminanceRange {
            low: 50.0,
            low_feather: feather,
            high: 90.0,
            high_feather: feather,
        });
        let mut worst = 0.0f64;
        let h = 1e-7;
        for i in 0..=20_000 {
            let e = 0.5 - feather / 100.0 + (feather / 100.0) * f64::from(i) / 20_000.0;
            let slope = (luminance_coverage_at(&band, e + h) - luminance_coverage_at(&band, e)) / h;
            worst = worst.max(slope.abs());
        }
        let bound = 150.0 / feather;
        assert!(
            worst <= bound * 1.001,
            "feather {feather}: measured slope {worst} above the bound {bound}"
        );
        assert!(
            worst > bound * 0.99,
            "feather {feather}: measured slope {worst} well below the bound {bound}"
        );
    }
}

// ---------------------------------------------------------------------------
// The colour range
// ---------------------------------------------------------------------------

#[test]
fn the_nearest_sample_form_equals_the_max_of_falloffs_bit_for_bit() {
    // The frozen spelling folds the nearest sample by `min` on the squared
    // distance and takes one square root; the natural spelling takes the
    // maximum of the per-sample falloffs and one square root per sample. They
    // are the same `f64` because the falloff is nonincreasing in `d` and both
    // evaluate it on the same value.
    let mut rng = SplitMix64(0x5EED_0031);
    let mut compared = 0u32;
    for _ in 0..300 {
        let payload = sample_colour_range(&mut rng);
        assert!(refine_is_legal(payload.refine));
        let compiled = compile_colour_range(&payload);
        for _ in 0..200 {
            let rgb = [
                rng.next_range(-0.1, 1.2),
                rng.next_range(-0.1, 1.2),
                rng.next_range(-0.1, 1.2),
            ];
            let lab = colour::to_oklab(rgb);
            let frozen = colour_coverage_at(&compiled, lab);
            let max_form = colour_coverage_max_form(&compiled, lab);
            assert_eq!(
                frozen.to_bits(),
                max_form.to_bits(),
                "{payload:?} at {rgb:?}: {frozen} vs {max_form}"
            );
            compared += 1;
        }
    }
    assert!(compared >= 60_000, "only {compared} points compared");
}

#[test]
fn duplicating_and_reordering_samples_changes_nothing_bit_for_bit() {
    // The multi-sample combination inherits the mask study's reason for Zadeh
    // over the product algebra: `min` is exact, idempotent and commutative, so
    // sampling the same colour twice and reordering the swatches are both
    // identities, bit for bit.
    let mut rng = SplitMix64(0x5EED_0032);
    let mut worst_product = 0.0f64;
    for _ in 0..300 {
        let payload = sample_colour_range(&mut rng);
        let compiled = compile_colour_range(&payload);

        let mut doubled = payload.clone();
        doubled.samples.push(payload.samples[0]);
        let doubled = compile_colour_range(&doubled);

        let mut shuffled = payload.clone();
        shuffled.samples.reverse();
        let shuffled = compile_colour_range(&shuffled);

        for _ in 0..80 {
            let rgb = [
                rng.next_range(0.0, 1.0),
                rng.next_range(0.0, 1.0),
                rng.next_range(0.0, 1.0),
            ];
            let lab = colour::to_oklab(rgb);
            let base = colour_coverage_at(&compiled, lab);
            assert_eq!(
                base.to_bits(),
                colour_coverage_at(&doubled, lab).to_bits(),
                "duplicating a sample changed coverage"
            );
            assert_eq!(
                base.to_bits(),
                colour_coverage_at(&shuffled, lab).to_bits(),
                "reordering the samples changed coverage"
            );
            // The product algebra fails the same way it failed for the mask's
            // component list, and the study records how far.
            let product = colour_coverage_product_form(&compiled, lab);
            let product_doubled = colour_coverage_product_form(&doubled, lab);
            worst_product = worst_product.max((product - product_doubled).abs());
        }
    }
    assert!(
        worst_product > 0.2,
        "the product form was expected to move by a visible amount, measured {worst_product}"
    );
}

#[test]
fn colour_coverage_is_exact_at_the_sample_at_the_plateau_and_at_the_radius() {
    let mut rng = SplitMix64(0x5EED_0033);
    for _ in 0..500 {
        let payload = sample_colour_range(&mut rng);
        let compiled = compile_colour_range(&payload);
        let sample = payload.samples[0];
        let lab = colour::to_oklab(sample);
        // Exactly 1 at a sampled colour.
        assert_eq!(colour_coverage_at(&compiled, lab), 1.0, "{payload:?}");
        // Only meaningful when this sample really is the nearest one.
        if compiled.points.len() == 1 {
            let probe = |offset: f64| Oklab {
                l: lab.l,
                a: lab.a + offset,
                b: lab.b,
            };
            // Exactly 1 out to the plateau's edge.
            assert_eq!(
                colour_coverage_at(&compiled, probe(PLATEAU * compiled.radius)),
                1.0,
                "{payload:?}"
            );
            // At the radius itself the probe's own distance is a rounded value,
            // so coverage is below 1e-28 rather than exactly zero; past it, it
            // is exactly zero.
            assert!(
                colour_coverage_at(&compiled, probe(compiled.radius)) < 1e-28,
                "{payload:?}"
            );
            assert_eq!(
                colour_coverage_at(&compiled, probe(compiled.radius * 1.000_001)),
                0.0,
                "{payload:?}"
            );
            assert_eq!(
                colour_coverage_at(&compiled, probe(compiled.radius * 1.5)),
                0.0,
                "{payload:?}"
            );
        }
    }
}

#[test]
fn colour_coverage_is_nonincreasing_with_distance_from_the_nearest_sample() {
    let mut rng = SplitMix64(0x5EED_0034);
    for _ in 0..200 {
        let payload = ColourRange {
            samples: vec![[
                rng.next_range(0.0, 1.0),
                rng.next_range(0.0, 1.0),
                rng.next_range(0.0, 1.0),
            ]],
            refine: rng.next_range(0.0, 100.0),
        };
        let compiled = compile_colour_range(&payload);
        let lab = colour::to_oklab(payload.samples[0]);
        let theta = rng.next_range(0.0, std::f64::consts::TAU);
        let mut previous = 1.0;
        for i in 0..=400 {
            let d = compiled.radius * 1.5 * f64::from(i) / 400.0;
            let probe = Oklab {
                l: lab.l,
                a: lab.a + d * theta.cos(),
                b: lab.b + d * theta.sin(),
            };
            let c = colour_coverage_at(&compiled, probe);
            assert!((0.0..=1.0).contains(&c), "{payload:?}: {c}");
            assert!(c <= previous + 1e-15, "{payload:?} rose at d={d}");
            previous = c;
        }
    }
}

#[test]
fn the_metric_is_blind_to_lightness_and_that_is_what_holds_a_shaded_surface() {
    // The frozen metric has no lightness term. This is the measurement that
    // settles it: for three real surfaces, the movement caused by a stop of
    // shading is compared with the distance to the nearest *different* surface
    // a person would not want selected.
    let cases = [
        ("blue sky", "blue flower"),
        ("light skin", "orange"),
        ("foliage", "green"),
    ];
    for (surface, intruder) in cases {
        let base = patch(surface);
        let base_lab = colour::to_oklab(base);
        let intruder_lab = colour::to_oklab(patch(intruder));
        for metric in [
            Metric::Chromaticity,
            Metric::Weighted(0.5),
            Metric::Weighted(1.0),
        ] {
            let mut shading = 0.0f64;
            for gain in [0.5, 0.7, 1.4, 2.0] {
                let lab = colour::to_oklab(shaded(base, gain));
                shading = shading.max(oklab_distance(metric, lab, base_lab));
            }
            let separation = oklab_distance(metric, intruder_lab, base_lab);
            let margin = separation / shading;
            match metric {
                // The frozen metric keeps the surface at least twice as close
                // to itself as the intruder is: a radius exists that takes all
                // of the surface and none of the intruder.
                Metric::Chromaticity => assert!(
                    margin > 2.0,
                    "{surface} vs {intruder}: chromaticity margin {margin} was not above 2"
                ),
                // Both weighted metrics lose that margin on at least one of the
                // three surfaces; the study prints the whole table.
                Metric::Weighted(w) => assert!(
                    margin.is_finite(),
                    "{surface} vs {intruder} at w={w}: {margin}"
                ),
            }
        }
    }

    // The specific failure the weighted metrics show, quoted in the study: a
    // sky that falls off by one stop moves further under a full Oklab distance
    // than a different blue flower sits from the sampled sky.
    let sky = patch("blue sky");
    let sky_lab = colour::to_oklab(sky);
    let shaded_sky = colour::to_oklab(shaded(sky, 2.0));
    let flower = colour::to_oklab(patch("blue flower"));
    let full = Metric::Weighted(1.0);
    assert!(
        oklab_distance(full, shaded_sky, sky_lab) > oklab_distance(full, flower, sky_lab),
        "the full Oklab distance was expected to fail this ordering"
    );
    let frozen = Metric::Chromaticity;
    assert!(
        oklab_distance(frozen, shaded_sky, sky_lab) < oklab_distance(frozen, flower, sky_lab),
        "the frozen metric must keep the ordering"
    );
}

#[test]
fn the_refine_mapping_spends_its_travel_on_the_radii_that_separate_surfaces() {
    // Geometric against linear. The measurements below put every decision a
    // person makes between a radius of about 0.05 (a whole colour family) and
    // 0.005 (one flat patch); the comparison is how much of the slider's travel
    // each mapping gives to that decade.
    let geometric_travel =
        100.0 - 100.0 * (0.05f64 / RADIUS_MAX).ln() / (RADIUS_MIN / RADIUS_MAX).ln();
    let linear_travel = 100.0 - 100.0 * (RADIUS_MAX - 0.05) / (RADIUS_MAX - RADIUS_MIN);
    assert!(
        (geometric_travel - 58.9).abs() < 0.1,
        "geometric travel moved: {geometric_travel}"
    );
    assert!(
        (linear_travel - 18.4).abs() < 0.1,
        "linear travel moved: {linear_travel}"
    );
    assert!(
        geometric_travel > 3.0 * linear_travel,
        "geometric {geometric_travel} against linear {linear_travel}"
    );

    // Both mappings share their ends, so the difference is entirely where the
    // travel is spent.
    assert!((refine_radius(0.0) - refine_radius_linear(0.0)).abs() < 1e-15);
    assert!((refine_radius(100.0) - refine_radius_linear(100.0)).abs() < 1e-15);

    // The mapping is strictly decreasing, so a higher refine is always a
    // tighter selection.
    let mut previous = f64::INFINITY;
    for i in 0..=1000 {
        let radius = refine_radius(f64::from(i) / 10.0);
        assert!(radius < previous, "refine {i} was not tighter");
        assert!((RADIUS_MIN..=RADIUS_MAX).contains(&radius));
        previous = radius;
    }

    // The default refine is the geometric mean of the two ends, which the
    // measurements below show is also, to within three points, the setting that
    // holds an ordinary surface across a stop of shading.
    let default_radius = refine_radius(50.0);
    assert!(
        (default_radius - 0.035_355).abs() < 1e-5,
        "{default_radius}"
    );
    for surface in ["light skin", "blue sky", "foliage"] {
        let base = patch(surface);
        let base_lab = colour::to_oklab(base);
        let mut hold = 0.0f64;
        for gain in [0.5, 2.0] {
            hold = hold.max(oklab_distance(
                Metric::Chromaticity,
                colour::to_oklab(shaded(base, gain)),
                base_lab,
            ));
        }
        // Full coverage reaches PLATEAU * radius, so holding the surface needs
        // a radius of twice its own spread.
        let needed = 2.0 * hold;
        assert!(
            needed <= default_radius * 1.02,
            "{surface} needs {needed}, the default gives {default_radius}"
        );
        let compiled = compile_colour_range(&ColourRange {
            samples: vec![base],
            refine: 50.0,
        });
        for gain in [0.5, 0.7, 1.0, 1.4, 2.0] {
            assert_eq!(
                colour_coverage(&compiled, shaded(base, gain)),
                1.0,
                "{surface} at gain {gain} was not fully selected at the default refine"
            );
        }
    }
    // `smooth` and `SPAN` are used by the figures below; keep the imports
    // honest rather than shaping the test around them.
    assert_eq!(smooth(1.0) / SPAN, 2.0);
}

#[test]
fn legal_payloads_never_produce_non_finite_coverage() {
    let mut rng = SplitMix64(0x5EED_0041);
    for _ in 0..500 {
        let band = compile_luminance_range(&sample_band(&mut rng));
        let range = compile_colour_range(&sample_colour_range(&mut rng));
        for _ in 0..100 {
            // Deliberately outside the gamut in both directions, which an
            // earlier unit in the same colour run may legitimately produce.
            let rgb = [
                rng.next_range(-2.0, 4.0),
                rng.next_range(-2.0, 4.0),
                rng.next_range(-2.0, 4.0),
            ];
            let cl = luminance_coverage(&band, rgb);
            let cc = colour_coverage(&range, rgb);
            assert!(cl.is_finite() && (0.0..=1.0).contains(&cl), "{rgb:?}: {cl}");
            assert!(cc.is_finite() && (0.0..=1.0).contains(&cc), "{rgb:?}: {cc}");
        }
    }
}

// ---------------------------------------------------------------------------
// Scenes chosen to fail
// ---------------------------------------------------------------------------

#[test]
fn the_luminance_band_cannot_separate_a_blue_sky_from_a_grey_card() {
    // The honest limit a luminance band has: it knows one number per pixel. A
    // photographed blue sky and a mid grey sit 1.4 output codes apart on the
    // axis, so no band selects one without the other.
    let sky = 100.0 * luminance_axis(patch("blue sky"));
    let grey = 100.0 * luminance_axis(patch("neutral 5"));
    assert!(
        (sky - grey).abs() < 1.0,
        "sky {sky} and grey {grey} were expected to be within one slider unit"
    );
    let skin = 100.0 * luminance_axis(patch("light skin"));
    let lighter_grey = 100.0 * luminance_axis(patch("neutral 6.5"));
    assert!(
        (skin - lighter_grey).abs() < 1.0,
        "skin {skin} and grey {lighter_grey} were expected to be within one slider unit"
    );

    // Both are fully selected by any band that selects either.
    let band = compile_luminance_range(&LuminanceRange {
        low: 40.0,
        low_feather: 5.0,
        high: 55.0,
        high_feather: 5.0,
    });
    assert_eq!(luminance_coverage(&band, patch("blue sky")), 1.0);
    assert_eq!(luminance_coverage(&band, patch("neutral 5")), 1.0);
}

#[test]
fn a_hard_band_speckles_on_a_noisy_shadow_and_a_soft_one_does_not() {
    // A range selection is a per-pixel value test, so it inherits the input's
    // noise. This measures the cost on a shadow at output code 40 with about
    // two codes of noise: the fraction of neighbouring pixels whose coverage
    // differs by more than half, for a hard edge and for the smallest and a
    // moderate legal shoulder.
    let mut results = Vec::new();
    for feather in [0.0, FEATHER_MIN, 10.0] {
        let band = compile_luminance_range(&LuminanceRange {
            low: 40.0 / 2.55,
            low_feather: feather,
            high: 100.0,
            high_feather: 0.0,
        });
        let mut rng = SplitMix64(0x5EED_0051);
        let mut flips = 0u32;
        let mut previous: Option<f64> = None;
        let total = 200_000u32;
        for _ in 0..total {
            let code = 40.0 + 2.0 * rng.next_noise();
            let linear = reference::srgb_decode(code.clamp(0.0, 255.0).round() as u8);
            let c = luminance_coverage(&band, [linear, linear, linear]);
            if let Some(p) = previous
                && (c - p).abs() > 0.5
            {
                flips += 1;
            }
            previous = Some(c);
        }
        results.push((feather, f64::from(flips) / f64::from(total)));
    }
    // The hard edge flips on a large fraction of neighbouring pixels; the
    // smallest legal shoulder is barely better, because it is only 2.55 codes
    // wide; a ten-unit shoulder removes the speckle entirely.
    assert!(
        results[0].1 > 0.2,
        "hard edge speckle {results:?} was expected to be substantial"
    );
    assert!(
        results[2].1 < 0.001,
        "a ten-unit shoulder still speckled: {results:?}"
    );
}

#[test]
fn no_refine_setting_holds_a_face_and_drops_the_oak_floor() {
    // The failure a colour range has on an ordinary interior portrait, stated
    // as the two windows it has to satisfy and cannot: the refine that holds a
    // face across a stop of shading is at most 53.0, and the refine that drops
    // an oak floor is above 55.1, so no setting does both.
    let skin = patch("light skin");
    let skin_lab = colour::to_oklab(skin);
    let refine_for =
        |radius: f64| 100.0 * (radius / RADIUS_MAX).ln() / (RADIUS_MIN / RADIUS_MAX).ln();

    let mut hold = 0.0f64;
    for gain in [0.5, 2.0] {
        hold = hold.max(oklab_distance(
            Metric::Chromaticity,
            colour::to_oklab(shaded(skin, gain)),
            skin_lab,
        ));
    }
    let holds_the_face = refine_for(2.0 * hold);
    let to_wood = oklab_distance(
        Metric::Chromaticity,
        colour::to_oklab(codes_to_linear(WOOD)),
        skin_lab,
    );
    let drops_the_floor = refine_for(to_wood);
    assert!((holds_the_face - 53.0).abs() < 0.2, "{holds_the_face}");
    assert!((drops_the_floor - 55.1).abs() < 0.2, "{drops_the_floor}");
    assert!(
        drops_the_floor > holds_the_face,
        "the two windows were expected not to overlap"
    );

    // At the default refine the face is whole and the floor is a third
    // selected, which under a masked +1 EV is plainly visible.
    let compiled = compile_colour_range(&ColourRange {
        samples: vec![skin],
        refine: 50.0,
    });
    assert_eq!(colour_coverage(&compiled, skin), 1.0);
    let wood_coverage = colour_coverage(&compiled, codes_to_linear(WOOD));
    assert!(
        (wood_coverage - 0.2955).abs() < 0.01,
        "oak floor coverage moved: {wood_coverage}"
    );
    let before = linear_to_code(codes_to_linear(WOOD)[0]);
    let after = code_after_masked_exposure(codes_to_linear(WOOD)[0], wood_coverage);
    assert!(
        i32::from(after) - i32::from(before) >= 20,
        "the floor moved only {before} -> {after} codes"
    );

    // Terracotta, further away, is the case that does work: it is out of the
    // selection at the default refine, so the limit is about how close a
    // surface is, not about the metric failing on every warm colour.
    let terracotta = colour_coverage(&compiled, codes_to_linear(TERRACOTTA));
    assert_eq!(terracotta, 0.0, "terracotta was expected to stay out");

    // The other half of the honest statement: a colour range cannot separate
    // one person's skin from another's. Dark skin is 0.0108 from light skin,
    // a third of what the lit face's own shading spans.
    let dark = oklab_distance(
        Metric::Chromaticity,
        colour::to_oklab(patch("dark skin")),
        skin_lab,
    );
    assert!((dark - 0.0108).abs() < 0.001, "{dark}");
    assert_eq!(colour_coverage(&compiled, patch("dark skin")), 1.0);
}

#[test]
fn every_neutral_is_one_colour_to_the_frozen_metric() {
    // The price of dropping the lightness term, stated as a measurement rather
    // than as a caveat: white, mid grey and black are mutually within a
    // thousandth of an Oklab unit, so a sampled grey selects the whole tonal
    // range and the remedy is a luminance range intersected with it.
    let names = ["white", "neutral 8", "neutral 6.5", "neutral 5", "black"];
    let labs: Vec<Oklab> = names.iter().map(|n| colour::to_oklab(patch(n))).collect();
    let mut worst = 0.0f64;
    for i in 0..labs.len() {
        for j in (i + 1)..labs.len() {
            worst = worst.max(oklab_distance(Metric::Chromaticity, labs[i], labs[j]));
        }
    }
    assert!(
        worst < 2.0 * RADIUS_MIN,
        "the neutrals were {worst} apart, which would make them separable"
    );

    // Sampling a mid grey at the tightest refine still selects black and white.
    let compiled = compile_colour_range(&ColourRange {
        samples: vec![patch("neutral 5")],
        refine: 100.0,
    });
    assert!(colour_coverage(&compiled, patch("white")) > 0.9);
    assert!(colour_coverage(&compiled, patch("black")) > 0.9);
}

#[test]
fn a_range_selection_at_proxy_scale_is_not_the_mean_of_the_exact_selection() {
    // The proxy consequence, measured. A value-based component reads whatever
    // pixel the stage hands it, so at Fit it reads a downscaled pixel: the
    // coverage of the average is not the average of the coverages, and no
    // supersample of the *mask* can recover it, because the full-resolution
    // pixels are not there to read. Measured at a sky/roof edge over a 2x2.
    let sky = patch("blue sky");
    let roof = codes_to_linear([90, 60, 50]);
    let averaged = [
        0.5 * (sky[0] + roof[0]),
        0.5 * (sky[1] + roof[1]),
        0.5 * (sky[2] + roof[2]),
    ];

    let band = study_band();
    let range = study_colour_range();
    for (label, exact_sky, exact_roof, proxy) in [
        (
            "luminance",
            luminance_coverage(&band, sky),
            luminance_coverage(&band, roof),
            luminance_coverage(&band, averaged),
        ),
        (
            "colour",
            colour_coverage(&range, sky),
            colour_coverage(&range, roof),
            colour_coverage(&range, averaged),
        ),
    ] {
        let mean = 0.5 * (exact_sky + exact_roof);
        let difference = (proxy - mean).abs();
        assert!(
            difference > 0.1,
            "{label}: proxy {proxy} and the exact mean {mean} were expected to differ visibly"
        );
    }
}

#[test]
fn the_selection_follows_the_operations_input_not_the_finished_frame() {
    // The other honest limit: a range component reads the pixel the operation
    // it modulates receives. A preceding exposure layer therefore moves the
    // selection, which is correct and is also surprising, so the study and the
    // user guide say it. Measured: a half-stop lift moves a sky from fully
    // selected to not selected at all under a band that was drawn for it.
    let band = compile_luminance_range(&LuminanceRange {
        low: 40.0,
        low_feather: 4.0,
        high: 52.0,
        high_feather: 4.0,
    });
    let sky = patch("blue sky");
    assert_eq!(luminance_coverage(&band, sky), 1.0);
    let lifted = shaded(sky, 2f64.powf(0.75));
    assert_eq!(luminance_coverage(&band, lifted), 0.0);
}

// ---------------------------------------------------------------------------
// Tolerance
// ---------------------------------------------------------------------------

#[test]
fn the_frozen_tolerance_is_under_one_output_code_at_every_legal_payload() {
    // The tolerance is frozen on the *metric* at the repository default and
    // derived on coverage through the shoulder's Lipschitz constant. This
    // asserts the derivation's own arithmetic and its conclusion in output
    // codes, so the study's claim is checked rather than asserted.
    let metric_tolerance = |value: f64| 1e-6 + 1e-6 * value.abs();

    // Luminance: the worst case is the narrowest legal shoulder, at an axis
    // value of 1.0.
    let epsilon_e = metric_tolerance(1.0);
    let worst_luminance = 1.5 * epsilon_e / (FEATHER_MIN / 100.0);
    assert!(
        worst_luminance < 5e-4,
        "luminance coverage bound {worst_luminance}"
    );

    // Colour: the worst case is the tightest refine, whose falloff spans
    // RADIUS_MIN * SPAN in Oklab units. The largest Oklab chromaticity distance
    // between two in-gamut colours bounds the relative term.
    let epsilon_d = metric_tolerance(0.6);
    let worst_colour = 1.5 * epsilon_d / (RADIUS_MIN * SPAN);
    assert!(worst_colour < 1e-3, "colour coverage bound {worst_colour}");

    // Both, expressed where a person can see them: the output-code difference a
    // coverage error of that size produces under a masked +1 EV, measured on the
    // unquantized encoded value over every input code so the answer is a
    // fraction of a code rather than a rounded one.
    let worst_fraction = |coverage_error: f64| -> f64 {
        let mut worst = 0.0f64;
        for code in 0u8..=255 {
            let input = srgb_to_linear(code);
            let blend =
                |c: f64| 255.0 * reference::srgb_encode((1.0 - c) * input + c * 2.0 * input);
            worst = worst.max((blend(0.5 + coverage_error) - blend(0.5)).abs());
        }
        worst
    };
    let luminance_codes = worst_fraction(worst_luminance);
    let colour_codes = worst_fraction(worst_colour);
    assert!(
        (luminance_codes - 0.0265).abs() < 0.001,
        "luminance code bound moved: {luminance_codes}"
    );
    assert!(
        (colour_codes - 0.0849).abs() < 0.001,
        "colour code bound moved: {colour_codes}"
    );

    // And the standing promise, asserted on the quantizer itself.
    let worst_coverage = worst_luminance.max(worst_colour);
    let mut worst_codes = 0i32;
    for code in 0u8..=255 {
        let input = srgb_to_linear(code);
        let a = code_after_masked_exposure(input, 0.5);
        let b = code_after_masked_exposure(input, 0.5 + worst_coverage);
        worst_codes = worst_codes.max(i32::from(a).abs_diff(i32::from(b)) as i32);
    }
    assert!(
        worst_codes <= 1,
        "a coverage error of {worst_coverage} moved {worst_codes} output codes"
    );
}

// ---------------------------------------------------------------------------
// Figures
// ---------------------------------------------------------------------------

#[test]
#[ignore = "prints the study's dense figures; not an assertion"]
fn range_study_figures() {
    println!("\n== the chart's patches on the frozen luminance axis and in Oklab ==");
    println!("patch            y(0..100)   L        a         b        C");
    for (name, codes) in CHART {
        let rgb = codes_to_linear(codes);
        let lab = colour::to_oklab(rgb);
        let c = (lab.a * lab.a + lab.b * lab.b).sqrt();
        println!(
            "{name:15} {:8.2}  {:.4}  {:+.4}  {:+.4}  {:.4}",
            100.0 * luminance_axis(rgb),
            lab.l,
            lab.a,
            lab.b,
            c
        );
    }

    println!("\n== the three candidate luminance axes, in slider units ==");
    println!("patch            frozen  encoded-luma  max-channel");
    for name in [
        "red",
        "blue",
        "yellow",
        "light skin",
        "blue sky",
        "neutral 5",
    ] {
        let rgb = patch(name);
        println!(
            "{name:15} {:6.2}      {:6.2}      {:6.2}",
            100.0 * axis_value(Axis::LinearLuminance, rgb),
            100.0 * axis_value(Axis::EncodedLuma, rgb),
            100.0 * axis_value(Axis::MaxChannel, rgb),
        );
    }

    println!("\n== the colour metric: shading against separation ==");
    println!("surface / intruder          metric        shading  separation  margin");
    for (surface, intruder) in [
        ("blue sky", "blue flower"),
        ("light skin", "orange"),
        ("foliage", "green"),
        ("light skin", "dark skin"),
    ] {
        let base = patch(surface);
        let base_lab = colour::to_oklab(base);
        let intruder_lab = colour::to_oklab(patch(intruder));
        for (label, metric) in [
            ("chromaticity", Metric::Chromaticity),
            ("weighted 0.5", Metric::Weighted(0.5)),
            ("weighted 1.0", Metric::Weighted(1.0)),
        ] {
            let mut shading = 0.0f64;
            for gain in [0.5, 0.7, 1.4, 2.0] {
                let lab = colour::to_oklab(shaded(base, gain));
                shading = shading.max(oklab_distance(metric, lab, base_lab));
            }
            let separation = oklab_distance(metric, intruder_lab, base_lab);
            println!(
                "{surface:11} / {intruder:11}  {label:12}  {shading:.4}   {separation:.4}     {:.2}",
                separation / shading
            );
        }
    }

    println!("\n== the refine mapping ==");
    println!("refine  geometric  linear   patches selected (geometric / linear)");
    let sky_lab = colour::to_oklab(patch("blue sky"));
    let selected = |radius: f64| -> usize {
        CHART
            .iter()
            .filter(|(_, codes)| {
                let lab = colour::to_oklab(codes_to_linear(*codes));
                let r = oklab_distance(Metric::Chromaticity, lab, sky_lab) / radius;
                smooth(((1.0 - r) / SPAN).clamp(0.0, 1.0)) > 0.0
            })
            .count()
    };
    for i in 0..=10 {
        let refine = f64::from(i) * 10.0;
        let g = refine_radius(refine);
        let l = refine_radius_linear(refine);
        println!(
            "{refine:6.0}  {g:.5}    {l:.5}  {:2} / {:2}",
            selected(g),
            selected(l)
        );
    }

    println!("\n== a sampled sky at the default refine ==");
    let compiled = study_colour_range();
    println!(
        "radius {:.5}, plateau {:.5}",
        compiled.radius,
        PLATEAU * compiled.radius
    );
    for (name, codes) in CHART {
        let c = colour_coverage(&compiled, codes_to_linear(codes));
        if c > 0.0 {
            println!("  selects {name:15} at {c:.6}");
        }
    }
    for gain in [0.35, 0.5, 0.7, 1.0, 1.4, 2.0, 2.8] {
        println!(
            "  the same sky at gain {gain:4.2}: {:.6}",
            colour_coverage(&compiled, shaded(patch("blue sky"), gain))
        );
    }

    println!("\n== what a sampled surface holds, and what comes with it ==");
    for (surface, base) in [
        ("light skin", patch("light skin")),
        ("blue sky", patch("blue sky")),
        ("foliage", patch("foliage")),
    ] {
        let base_lab = colour::to_oklab(base);
        let mut hold = 0.0f64;
        for gain in [0.5, 2.0] {
            hold = hold.max(oklab_distance(
                Metric::Chromaticity,
                colour::to_oklab(shaded(base, gain)),
                base_lab,
            ));
        }
        println!(
            "{surface}: holds across +-1 stop with radius >= {:.4} (refine <= {:.1})",
            2.0 * hold,
            100.0 * ((2.0 * hold) / RADIUS_MAX).ln() / (RADIUS_MIN / RADIUS_MAX).ln()
        );
        let mut intruders: Vec<(&str, f64)> = CHART
            .iter()
            .filter(|(n, _)| *n != surface)
            .map(|(n, c)| {
                (
                    *n,
                    oklab_distance(
                        Metric::Chromaticity,
                        colour::to_oklab(codes_to_linear(*c)),
                        base_lab,
                    ),
                )
            })
            .collect();
        intruders.push((
            "oak floor",
            oklab_distance(
                Metric::Chromaticity,
                colour::to_oklab(codes_to_linear(WOOD)),
                base_lab,
            ),
        ));
        intruders.push((
            "terracotta",
            oklab_distance(
                Metric::Chromaticity,
                colour::to_oklab(codes_to_linear(TERRACOTTA)),
                base_lab,
            ),
        ));
        intruders.sort_by(|a, b| a.1.total_cmp(&b.1));
        for (name, d) in intruders.iter().take(6) {
            let refine_out = 100.0 * (d / RADIUS_MAX).ln() / (RADIUS_MIN / RADIUS_MAX).ln();
            println!("   {name:15} at {d:.4}; leaves the selection above refine {refine_out:.1}");
        }
    }

    println!("\n== the rejected product combination, over randomized ranges ==");
    {
        let mut rng = SplitMix64(0x5EED_0032);
        let mut worst = 0.0f64;
        for _ in 0..300 {
            let payload = sample_colour_range(&mut rng);
            let compiled = compile_colour_range(&payload);
            let mut doubled = payload.clone();
            doubled.samples.push(payload.samples[0]);
            let doubled = compile_colour_range(&doubled);
            for _ in 0..80 {
                let rgb = [
                    rng.next_range(0.0, 1.0),
                    rng.next_range(0.0, 1.0),
                    rng.next_range(0.0, 1.0),
                ];
                let lab = colour::to_oklab(rgb);
                worst = worst.max(
                    (colour_coverage_product_form(&compiled, lab)
                        - colour_coverage_product_form(&doubled, lab))
                    .abs(),
                );
            }
        }
        println!("duplicating one sample moves the product form by up to {worst:.6}");
    }

    println!("\n== the refine travel spent on the useful radius decade ==");
    let travel = |radius: f64, geometric: bool| -> f64 {
        if geometric {
            100.0 * (radius / RADIUS_MAX).ln() / (RADIUS_MIN / RADIUS_MAX).ln()
        } else {
            100.0 * (RADIUS_MAX - radius) / (RADIUS_MAX - RADIUS_MIN)
        }
    };
    println!(
        "geometric: radii 0.05 to {RADIUS_MIN} span refine {:.1} to 100, {:.1} points",
        travel(0.05, true),
        100.0 - travel(0.05, true)
    );
    println!(
        "linear:    radii 0.05 to {RADIUS_MIN} span refine {:.1} to 100, {:.1} points",
        travel(0.05, false),
        100.0 - travel(0.05, false)
    );

    println!("\n== noise on a shadow at output code 40 ==");
    println!("feather  coverage sd  neighbour flips");
    for feather in [0.0, FEATHER_MIN, 2.0, 5.0, 10.0] {
        let band = compile_luminance_range(&LuminanceRange {
            low: 40.0 / 2.55,
            low_feather: feather,
            high: 100.0,
            high_feather: 0.0,
        });
        let mut rng = SplitMix64(0x5EED_0051);
        let (mut sum, mut sum2, mut flips) = (0.0f64, 0.0f64, 0u32);
        let total = 200_000u32;
        let mut previous: Option<f64> = None;
        for _ in 0..total {
            let code = (40.0 + 2.0 * rng.next_noise()).clamp(0.0, 255.0).round() as u8;
            let linear = srgb_to_linear(code);
            let c = luminance_coverage(&band, [linear, linear, linear]);
            sum += c;
            sum2 += c * c;
            if let Some(p) = previous
                && (c - p).abs() > 0.5
            {
                flips += 1;
            }
            previous = Some(c);
        }
        let n = f64::from(total);
        let sd = (sum2 / n - (sum / n) * (sum / n)).max(0.0).sqrt();
        println!("{feather:7.1}  {sd:11.4}  {:.4}", f64::from(flips) / n);
    }

    println!("\n== proxy against exact at a sky/roof edge ==");
    let sky = patch("blue sky");
    let roof = codes_to_linear([90, 60, 50]);
    let averaged = [
        0.5 * (sky[0] + roof[0]),
        0.5 * (sky[1] + roof[1]),
        0.5 * (sky[2] + roof[2]),
    ];
    let band = study_band();
    let range = study_colour_range();
    println!(
        "luminance: sky {:.4}, roof {:.4}, mean {:.4}, proxy {:.4}",
        luminance_coverage(&band, sky),
        luminance_coverage(&band, roof),
        0.5 * (luminance_coverage(&band, sky) + luminance_coverage(&band, roof)),
        luminance_coverage(&band, averaged)
    );
    println!(
        "colour:    sky {:.4}, roof {:.4}, mean {:.4}, proxy {:.4}",
        colour_coverage(&range, sky),
        colour_coverage(&range, roof),
        0.5 * (colour_coverage(&range, sky) + colour_coverage(&range, roof)),
        colour_coverage(&range, averaged)
    );

    println!("\n== shoulder slope against the feather floor ==");
    for feather in [FEATHER_MIN, 2.0, 5.0, 15.0, 50.0, FEATHER_MAX] {
        println!(
            "feather {feather:5.1}: max |dc/de| = {:8.3} per unit of the axis, \
             {:.4} of coverage per output code",
            150.0 / feather,
            150.0 / feather / 255.0
        );
    }

    println!("\n== the largest in-gamut chromaticity distance ==");
    let mut worst = 0.0f64;
    let mut worst_pair = ("", "");
    for (an, ac) in CHART {
        for (bn, bc) in CHART {
            let d = oklab_distance(
                Metric::Chromaticity,
                colour::to_oklab(codes_to_linear(ac)),
                colour::to_oklab(codes_to_linear(bc)),
            );
            if d > worst {
                worst = d;
                worst_pair = (an, bn);
            }
        }
    }
    println!(
        "chart: {:.4} between {} and {}; RADIUS_MAX is {RADIUS_MAX}",
        worst, worst_pair.0, worst_pair.1
    );
    let corners: [[u8; 3]; 6] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 0],
        [0, 255, 255],
        [255, 0, 255],
    ];
    let mut worst_corner = 0.0f64;
    for a in corners {
        for b in corners {
            worst_corner = worst_corner.max(oklab_distance(
                Metric::Chromaticity,
                colour::to_oklab(codes_to_linear(a)),
                colour::to_oklab(codes_to_linear(b)),
            ));
        }
    }
    println!("gamut corners: {worst_corner:.4}");
}
