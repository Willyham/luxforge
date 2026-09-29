//! The kind-conformance suite: the one checklist every mask component kind passes, run through one
//! adapter per kind.
//!
//! A kind's coverage is transcribed twice — the production unit in `luxforge-core` and an
//! independent `f64` reference in `luxforge-reference` (`mask.rs` for the geometric kinds,
//! `range.rs` for the range selections), which share no code — and the bar is **bit-identity**
//! rather than a tolerance: the production unit writes the reference's expressions in the
//! reference's order, so the same payloads, stage and pixel give the same `f64` bits. A failure of
//! the sweeps here is a rewritten expression, not float noise. What the studies do not freeze — the
//! conservative `bounds` rectangle and `min_feature_px` — is held here by exhaustive evaluation on
//! small stages and by counted ramps.
//!
//! Each adapter (one file beside this one) states its kind's side of the checklist: how to draw a
//! random legal payload and its reference falloff, the payloads that select nothing or whose
//! inversion covers the whole frame, a feathered payload, its refusals, its measured ramps, its
//! glyph, menu title and band control, the seeds its sweeps draw from and the floors those seeds
//! recorded. [`checklist`] turns an adapter into one test per row, so a failure is named by its kind
//! and its row (`kinds::radial::bounds_never_exclude_a_non_zero_pixel`). A new kind is one adapter
//! and one entry in [`adapters`]; [`every_registered_kind_passes_the_checklist`] fails until it has
//! both.
//!
//! The rows:
//!
//! * the kind table claims the kind, names it, draws it with its glyph, offers it by its menu title,
//!   declares its band control and says whether it is value-based ([`claims_its_kind`]);
//! * composed masks of the kind — every mode, both inversions, any amount — are bit-identical to the
//!   frozen fold over the reference, sparsely on photo-sized stages and at every pixel of small
//!   ones, and one component alone is its reference falloff ([`is_bit_identical_to_the_reference`],
//!   [`is_bit_identical_over_whole_small_stages`], [`one_component_alone_is_its_reference_falloff`]);
//! * `evaluate` is the `f64` field narrowed once and stays in `[0, 1]`
//!   ([`evaluate_is_the_narrowed_field_and_stays_in_range`]);
//! * `bounds` never excludes a non-zero pixel, and is narrower than the stage often enough that the
//!   property is not vacuous ([`bounds_never_exclude_a_non_zero_pixel`]);
//! * a shape drawn off the frame bounds to nothing, an inverted drawn shape and a value-based one to
//!   the whole stage, and an amount of zero to nothing ([`the_rectangle_is_empty_or_whole_where_it_must_be`]);
//! * validation names the component and the field ([`refusals_name_what_they_refuse`]);
//! * `min_feature_px` is the measured ramp, at the stage it is asked about, and a mask's is its
//!   narrowest component's ([`min_feature_px_is_the_measured_ramp`]);
//! * a position-only kind ignores the pixel exactly and a value-based one ignores the position
//!   exactly — proposal P12's condition ([`reads_exactly_what_its_kind_says`]);
//! * a sampled byte equals the rendered byte through a masked layer, on the byte and the linear
//!   paths ([`a_sampled_byte_equals_the_rendered_byte`]).
//!
//! Beside the rows, [`kinds_meet_only_through_the_fold`] composes components of every kind in one
//! mask, and [`an_unknown_kind_is_refused_by_name_and_still_reads_back`] is the table's answer for a
//! kind no adapter claims.

mod brush;
pub(super) mod colour_range;
mod linear;
pub(super) mod luminance_range;
mod radial;

use super::*;
use luxforge_core::{
    ErrorKind, LinearSettings, ModuleRegistry, SnapshotId,
    mask::{self, CompiledMask},
    path::StrokeTable,
};
use luxforge_reference::mask::{Algebra, Mode, combine};
use luxforge_testkit::fixtures::{render, render_linear, sample, sample_linear};
use std::ops::RangeInclusive;

/// One component's reference falloff at a mask-space point and the pixel the masked operation
/// receives, before its own inversion and before the fold.
type Falloff = Box<dyn Fn(&RefStage, f64, f64, [f64; 3]) -> f64>;

/// A kind's random payload drawn in both spellings: the stored payload (its strokes, for a drawn
/// kind, inserted into the table) and its reference falloff.
type Sampler = fn(&mut SplitMix64, &[(u32, u32)], &mut StrokeTable) -> (Value, Falloff);

/// The small stages the dense sweeps walk at every pixel, so the clamps' two ends and the exactly
/// zero and exactly one plateaus are swept rather than sampled.
const SMALL: [(u32, u32); 4] = [(31, 17), (17, 31), (64, 64), (48, 12)];

/// The stages the conservative rectangle is checked on exhaustively.
const BOUNDS: [(u32, u32); 4] = [(23, 19), (19, 23), (32, 32), (48, 12)];

/// The seeds one kind's sweeps draw from. They are the per-kind tests' own seeds where the suite
/// replaced one, so the recorded floors below are figures and not guesses.
struct Seeds {
    sparse: u64,
    dense: u64,
    alone: u64,
    evaluate: u64,
    bounds: u64,
}

/// What `min_feature_px` must answer for one ramp.
enum Expect {
    Exact(f32),
    Near(f64, f64),
    Below(f32),
    Positive,
    Infinite,
}

/// One measured ramp: a payload compiled at one stage, asked about at another, and optionally the
/// number of rows its transition occupies down one column of the compiled stage.
struct Ramp {
    payload: Value,
    compiled: (u32, u32),
    asked: (u32, u32),
    expect: Expect,
    counted: Option<(u32, RangeInclusive<usize>)>,
}

/// One kind's side of the checklist.
trait Kind {
    /// The token the kind table claims.
    const KIND: &'static str;
    /// Its display name, which a component's name is built from.
    const TITLE: &'static str;
    /// The glyph a client draws it with.
    const ICON: &'static str;
    /// What a menu offering it calls it.
    const MENU_TITLE: &'static str;
    /// Whether its coverage is a function of the pixel's value rather than its position.
    const VALUE_BASED: bool;
    /// The label of the `range` control it declares over its patch method, when it is a band.
    const BAND: Option<&'static str> = None;
    const SEEDS: Seeds;
    /// The fewest rectangles of the bounds sweep that must be strictly smaller than their stage: a
    /// figure recorded at `SEEDS.bounds`, so a change that returned the whole stage unconditionally
    /// — which would pass the property and lose the point of it — fails instead.
    const NARROWER: usize;
    /// How many payloads the alone sweep draws, and the fewest partially covered answers it must
    /// reach, recorded at `SEEDS.alone`, so the sweep is not all endpoints.
    const ALONE_ROUNDS: usize;
    const ALONE_PARTIAL: usize;
    /// The fewest payloads of the alone sweep [`Kind::rare`] must accept.
    const RARE: usize = 0;

    /// A random legal payload, legal on every one of `stages`, and its reference falloff.
    fn sample(
        rng: &mut SplitMix64,
        stages: &[(u32, u32)],
        strokes: &mut StrokeTable,
    ) -> (Value, Falloff);
    /// A case of [`Kind::sample`] the alone sweep must reach [`Kind::RARE`] times.
    fn rare(_payload: &Value) -> bool {
        false
    }
    /// Payloads that select nothing anywhere on a 40 × 30 frame. None for a value-based kind, which
    /// has no position to put off the frame.
    fn nothing(strokes: &mut StrokeTable) -> Vec<Value>;
    /// A payload drawn inside a 40 × 30 frame whose inversion is non-zero at every corner, with a
    /// pixel where that inversion is exactly zero; none for a kind whose inverse is itself bounded.
    fn inverted_whole(strokes: &mut StrokeTable) -> Option<(Value, (u32, u32))>;
    /// A payload whose transition covers much of the small synthetic stage.
    fn feathered(strokes: &mut StrokeTable) -> Value;
    /// Every refusal a malformed or illegal payload produces on a 400 × 400 stage, in full.
    fn refusals() -> Vec<(Value, ErrorKind, &'static str)>;
    /// Payloads at the edges of legality, each of which compiles on a 400 × 400 stage.
    fn legal() -> Vec<Value>;
    /// The ramps whose width `min_feature_px` states.
    fn ramps(strokes: &mut StrokeTable) -> Vec<Ramp>;
}

/// One row of the checklist per test, for the kind `$kind`, in the adapter's own module.
macro_rules! checklist {
    ($kind:ty) => {
        #[test]
        fn claims_its_kind() {
            super::claims_its_kind::<$kind>();
        }
        #[test]
        fn is_bit_identical_to_the_reference() {
            super::is_bit_identical_to_the_reference::<$kind>();
        }
        #[test]
        fn is_bit_identical_over_whole_small_stages() {
            super::is_bit_identical_over_whole_small_stages::<$kind>();
        }
        #[test]
        fn one_component_alone_is_its_reference_falloff() {
            super::one_component_alone_is_its_reference_falloff::<$kind>();
        }
        #[test]
        fn evaluate_is_the_narrowed_field_and_stays_in_range() {
            super::evaluate_is_the_narrowed_field_and_stays_in_range::<$kind>();
        }
        #[test]
        fn bounds_never_exclude_a_non_zero_pixel() {
            super::bounds_never_exclude_a_non_zero_pixel::<$kind>();
        }
        #[test]
        fn the_rectangle_is_empty_or_whole_where_it_must_be() {
            super::the_rectangle_is_empty_or_whole_where_it_must_be::<$kind>();
        }
        #[test]
        fn refusals_name_what_they_refuse() {
            super::refusals_name_what_they_refuse::<$kind>();
        }
        #[test]
        fn min_feature_px_is_the_measured_ramp() {
            super::min_feature_px_is_the_measured_ramp::<$kind>();
        }
        #[test]
        fn reads_exactly_what_its_kind_says() {
            super::reads_exactly_what_its_kind_says::<$kind>();
        }
        #[test]
        fn a_sampled_byte_equals_the_rendered_byte() {
            super::a_sampled_byte_equals_the_rendered_byte::<$kind>();
        }
    };
}
use checklist;

/// Every adapter, in the kind table's order: its kind and its sampler.
fn adapters() -> [(&'static str, Sampler); 5] {
    fn of<K: Kind>() -> (&'static str, Sampler) {
        (K::KIND, K::sample)
    }
    [
        of::<linear::Linear>(),
        of::<radial::Radial>(),
        of::<brush::Brush>(),
        of::<luminance_range::LuminanceRange>(),
        of::<colour_range::ColourRange>(),
    ]
}

// ---------------------------------------------------------------------------
// Shared machinery
// ---------------------------------------------------------------------------

/// A randomized linear pixel, including values outside `[0, 1]`: the luminance axis is deliberately
/// unclamped and the Oklab conversion deliberately signed, so a sweep has to reach both.
fn sample_pixel(rng: &mut SplitMix64) -> [f64; 3] {
    [
        rng.next_range(-0.5, 2.5),
        rng.next_range(-0.5, 2.5),
        rng.next_range(-0.5, 2.5),
    ]
}

/// The pixel a sweep hands kind `K`: a random one for a value-based kind, and the arbitrary fixed
/// one a position-only kind ignores (drawing none, so its sweep's sequence is its geometry alone).
fn pixel<K: Kind>(rng: &mut SplitMix64) -> [f64; 3] {
    if K::VALUE_BASED {
        sample_pixel(rng)
    } else {
        ANY_PIXEL
    }
}

/// The reference side of a mask: the frozen Zadeh fold of `docs/design/mask-study.md#composition`
/// over each component's reference falloff, in the order a production unit transcribes it.
struct Fold {
    amount: f64,
    invert: bool,
    components: Vec<(Mode, bool, Falloff)>,
}

impl Fold {
    fn coverage(&self, stage: &RefStage, x: u32, y: u32, rgb: [f64; 3]) -> f64 {
        let (u, v) = stage.pixel_uv(x, y);
        let mut m = 0.0;
        for (mode, invert, falloff) in &self.components {
            let c = falloff(stage, u, v, rgb);
            let c = if *invert { 1.0 - c } else { c };
            m = combine(Algebra::Zadeh, m, *mode, c);
        }
        let m = if self.invert { 1.0 - m } else { m };
        (self.amount / 100.0) * m
    }
}

/// One randomized mask in both spellings, with the strokes its drawn components reference.
struct Sampled {
    mask: Mask,
    strokes: StrokeTable,
    fold: Fold,
    kinds: Vec<&'static str>,
}

/// A randomized mask of `components` components, each drawn by `draw`: a random amount and
/// inversion, then per component its mode (the first is always `add`, which the model validates
/// structurally), its payload and its own inversion.
fn sample_mask(
    rng: &mut SplitMix64,
    components: usize,
    mut draw: impl FnMut(&mut SplitMix64, &mut StrokeTable) -> (&'static str, Value, Falloff),
) -> Sampled {
    let mut mask = Mask::new("Mask 1");
    let mut strokes = StrokeTable::new("the kind-conformance suite");
    mask.amount = rng.next_range(0.0, 100.0);
    mask.invert = rng.next_bool();
    let mut fold = Fold {
        amount: mask.amount,
        invert: mask.invert,
        components: Vec::new(),
    };
    let mut kinds = Vec::new();
    for index in 0..components {
        let (mode, ref_mode) = if index == 0 {
            (ComponentMode::Add, Mode::Add)
        } else {
            mode_of(rng.next_usize(3))
        };
        let (kind, payload, falloff) = draw(rng, &mut strokes);
        let invert = rng.next_bool();
        let name = mask.next_component_name(kind);
        let mut component = Component::new(name, mode, kind, payload);
        component.invert = invert;
        mask.components.push(component);
        fold.components.push((ref_mode, invert, falloff));
        kinds.push(kind);
    }
    Sampled {
        mask,
        strokes,
        fold,
        kinds,
    }
}

/// A randomized mask whose components are all of kind `K`, legal on every one of `stages`.
fn sample_of<K: Kind>(rng: &mut SplitMix64, components: usize, stages: &[(u32, u32)]) -> Sampled {
    sample_mask(rng, components, |rng, strokes| {
        let (payload, falloff) = K::sample(rng, stages, strokes);
        (K::KIND, payload, falloff)
    })
}

fn compile(mask: &Mask, (width, height): (u32, u32), strokes: &StrokeTable) -> CompiledMask {
    CompiledMask::new(mask, stage(width, height), strokes)
        .unwrap_or_else(|error| panic!("{} on {width}x{height}: {error}", mask.components[0].kind))
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// The kind table claims the kind and says everything a client asks of it without matching on its
/// token: its name, glyph, menu title, band control and whether it is value-based. A stored
/// component of the kind is named from its title, validates without a stage, and round-trips byte
/// for byte, which is what retention means for a kind this build does know.
fn claims_its_kind<K: Kind>() {
    let kind = K::KIND;
    assert!(mask::knows_component_kind(kind), "{kind}");
    assert_eq!(mask::kind_title(kind), K::TITLE, "{kind}");
    assert_eq!(mask::kind_icon(kind), Some(K::ICON), "{kind}");
    assert_eq!(mask::kind_menu_title(kind), K::MENU_TITLE, "{kind}");
    assert_eq!(
        mask::component_kind_is_value_based(kind),
        K::VALUE_BASED,
        "{kind}"
    );
    // The band control is published on the host descriptor over the kind's own patch method, and
    // only a band declares one.
    let patch = format!("mask.set-{kind}");
    let bands: Vec<String> = ModuleRegistry::builtin()
        .host_descriptors()
        .iter()
        .flat_map(|descriptor| &descriptor.controls)
        .map(|control| serde_json::to_value(control).expect("a control serializes"))
        .filter(|control| control["kind"] == json!("range") && control["action"] == json!(patch))
        .map(|control| control["label"].as_str().expect("a label").to_owned())
        .collect();
    assert_eq!(
        bands,
        K::BAND.into_iter().collect::<Vec<_>>(),
        "{kind}: the band controls over {patch}"
    );
    let mut strokes = StrokeTable::new("the kind-conformance suite");
    let stored = one_component(kind, K::feathered(&mut strokes));
    assert_eq!(stored.components[0].name, format!("{} 1", K::TITLE));
    stored.validate().unwrap();
    mask::validate_component_kinds(&stored)
        .unwrap_or_else(|error| panic!("{kind}: the stage-free check refused it: {error}"));
    let encoded = serde_json::to_vec(&stored).unwrap();
    let reopened: Mask = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(reopened, stored, "{kind}");
    assert_eq!(
        serde_json::to_string(&reopened.components[0].payload).unwrap(),
        serde_json::to_string(&stored.components[0].payload).unwrap(),
        "{kind}: the payload's bytes"
    );
}

/// The production field against the frozen fold over the reference, bit for bit, over randomized
/// masks of the kind on the photo-sized stages. `to_bits` rather than `==` so a `-0.0` or a NaN
/// could not pass as equal, and the assertion prints both patterns when it fails.
fn is_bit_identical_to_the_reference<K: Kind>() {
    let kind = K::KIND;
    let mut rng = SplitMix64(K::SEEDS.sparse);
    let mut checked = 0usize;
    for components in 1..=6 {
        for round in 0..40 {
            let sampled = sample_of::<K>(&mut rng, components, &STAGES);
            sampled
                .mask
                .validate()
                .expect("the sampled mask is structurally valid");
            for (width, height) in STAGES {
                let compiled = compile(&sampled.mask, (width, height), &sampled.strokes);
                let reference_stage = ref_stage(width, height);
                for _ in 0..24 {
                    let x = rng.next_usize(width as usize) as u32;
                    let y = rng.next_usize(height as usize) as u32;
                    let rgb = pixel::<K>(&mut rng);
                    let expected = sampled.fold.coverage(&reference_stage, x, y, rgb);
                    let actual = compiled.coverage(x, y, rgb);
                    assert_eq!(
                        actual.to_bits(),
                        expected.to_bits(),
                        "{kind}: {components} components, round {round}, {width}x{height} at \
                         ({x}, {y}), pixel {rgb:?}: {actual:?} ({:#018x}) against the reference \
                         {expected:?} ({:#018x})",
                        actual.to_bits(),
                        expected.to_bits()
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 6 * 40 * 4 * 24, "the sweep's size is stated");
}

/// The same comparison walked at every pixel of the small stages.
fn is_bit_identical_over_whole_small_stages<K: Kind>() {
    let kind = K::KIND;
    let mut rng = SplitMix64(K::SEEDS.dense);
    let mut checked = 0usize;
    for components in 1..=4 {
        for _ in 0..20 {
            let sampled = sample_of::<K>(&mut rng, components, &SMALL);
            for (width, height) in SMALL {
                let compiled = compile(&sampled.mask, (width, height), &sampled.strokes);
                let reference_stage = ref_stage(width, height);
                for y in 0..height {
                    for x in 0..width {
                        let rgb = pixel::<K>(&mut rng);
                        assert_eq!(
                            compiled.coverage(x, y, rgb).to_bits(),
                            sampled.fold.coverage(&reference_stage, x, y, rgb).to_bits(),
                            "{kind}: {width}x{height} at ({x}, {y}), pixel {rgb:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    let per_mask: usize = SMALL.iter().map(|(w, h)| (w * h) as usize).sum();
    assert_eq!(checked, 4 * 20 * per_mask, "the sweep's size is stated");
}

/// One component alone, with no composition in the way, against the kind's own reference falloff,
/// so a transcription failure is attributed to the falloff rather than to the fold. A position-only
/// kind is asked at random positions of the photo-sized stages; a value-based kind at random pixels
/// and three positions of a small stage, which must all answer alike.
fn one_component_alone_is_its_reference_falloff<K: Kind>() {
    let kind = K::KIND;
    let mut rng = SplitMix64(K::SEEDS.alone);
    let (mut partial, mut rare) = (0usize, 0usize);
    for round in 0..K::ALONE_ROUNDS {
        let mut strokes = StrokeTable::new("the kind-conformance suite");
        let (payload, falloff) = K::sample(&mut rng, &STAGES, &mut strokes);
        if K::rare(&payload) {
            rare += 1;
        }
        let mask = one_component(kind, payload);
        let compare = |compiled: &CompiledMask,
                       reference_stage: &RefStage,
                       (x, y): (u32, u32),
                       rgb: [f64; 3]| {
            let (u, v) = reference_stage.pixel_uv(x, y);
            let expected = falloff(reference_stage, u, v, rgb);
            let actual = compiled.coverage(x, y, rgb);
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{kind}: round {round}, {:?} at ({x}, {y}), pixel {rgb:?}: {actual:?} against \
                 {expected:?}",
                mask.components[0].payload
            );
            expected
        };
        if K::VALUE_BASED {
            // Position is irrelevant to this kind, so it is varied on purpose: a production unit
            // that let the position leak into the value would fail here and nowhere else.
            let compiled = compile(&mask, (8, 8), &strokes);
            let reference_stage = ref_stage(8, 8);
            for _ in 0..40 {
                let rgb = sample_pixel(&mut rng);
                let mut expected = 0.0;
                for at in [(0u32, 0u32), (3, 5), (7, 7)] {
                    expected = compare(&compiled, &reference_stage, at, rgb);
                }
                if expected > 0.0 && expected < 1.0 {
                    partial += 1;
                }
            }
        } else {
            for (width, height) in STAGES {
                let compiled = compile(&mask, (width, height), &strokes);
                let reference_stage = ref_stage(width, height);
                for _ in 0..40 {
                    let x = rng.next_usize(width as usize) as u32;
                    let y = rng.next_usize(height as usize) as u32;
                    let expected = compare(&compiled, &reference_stage, (x, y), ANY_PIXEL);
                    if expected > 0.0 && expected < 1.0 {
                        partial += 1;
                    }
                }
            }
        }
    }
    println!("{kind}: {partial} partially covered answers, {rare} rare payloads");
    assert!(
        partial >= K::ALONE_PARTIAL,
        "{kind}: only {partial} partially covered answers against a recorded {}: the sweep is all \
         endpoints",
        K::ALONE_PARTIAL
    );
    assert!(
        rare >= K::RARE,
        "{kind}: the sweep reached its rare case {rare} times against a recorded {}",
        K::RARE
    );
}

/// `evaluate` is the `f64` field narrowed once at the end, not an `f32` composition: it equals
/// `coverage(...) as f32` at every pixel of a small stage, over composed masks. And the field is
/// total on legal payloads: asked far across a 24 MP stage, one component still answers a finite
/// number in `[0, 1]`, which is what lets the stored ranges be validation rules and nothing more.
fn evaluate_is_the_narrowed_field_and_stays_in_range<K: Kind>() {
    let kind = K::KIND;
    let mut rng = SplitMix64(K::SEEDS.evaluate);
    for components in 1..=5 {
        for _ in 0..20 {
            let sampled = sample_of::<K>(&mut rng, components, &[(41, 29)]);
            let compiled = compile(&sampled.mask, (41, 29), &sampled.strokes);
            for y in 0..29 {
                for x in 0..41 {
                    let narrow = pixel::<K>(&mut rng).map(|channel| channel as f32);
                    let coverage = compiled.coverage(x, y, narrow.map(f64::from));
                    assert_eq!(
                        compiled.evaluate(x, y, narrow).to_bits(),
                        (coverage as f32).to_bits(),
                        "{kind} at ({x}, {y})"
                    );
                    assert!(
                        (0.0..=1.0).contains(&coverage),
                        "{kind}: {coverage} at ({x}, {y})"
                    );
                }
            }
        }
    }
    for _ in 0..200 {
        let mut strokes = StrokeTable::new("the kind-conformance suite");
        let (payload, _) = K::sample(&mut rng, &STAGES, &mut strokes);
        let mask = one_component(kind, payload);
        let compiled = compile(&mask, (6000, 4000), &strokes);
        for _ in 0..40 {
            let x = rng.next_usize(6000) as u32;
            let y = rng.next_usize(4000) as u32;
            let rgb = pixel::<K>(&mut rng);
            let coverage = compiled.coverage(x, y, rgb);
            assert!(
                coverage.is_finite() && (0.0..=1.0).contains(&coverage),
                "{kind}: {:?} at ({x}, {y}) gave {coverage}",
                mask.components[0].payload
            );
        }
    }
}

/// `bounds` is conservative: on stages small enough to evaluate exhaustively, no pixel outside the
/// rectangle has non-zero coverage, over randomized masks in every mode with inversions at both
/// levels. It also counts how often the rectangle is smaller than the stage and holds the count to
/// the kind's recorded floor, so a rectangle that quietly became the whole frame fails here rather
/// than passing vacuously. A value-based kind's rectangle is the whole stage every time, because a
/// value it selects can appear at any pixel.
fn bounds_never_exclude_a_non_zero_pixel<K: Kind>() {
    let kind = K::KIND;
    let mut rng = SplitMix64(K::SEEDS.bounds);
    let mut narrower = 0usize;
    let mut cases = 0usize;
    for components in 1..=5 {
        for _ in 0..60 {
            let sampled = sample_of::<K>(&mut rng, components, &BOUNDS);
            for (width, height) in BOUNDS {
                let compiled = compile(&sampled.mask, (width, height), &sampled.strokes);
                let bounds = compiled.bounds();
                cases += 1;
                if bounds.pixels() < u64::from(width) * u64::from(height) {
                    narrower += 1;
                }
                if K::VALUE_BASED {
                    assert_eq!(
                        (bounds.x0, bounds.y0, bounds.width, bounds.height),
                        (0, 0, width, height),
                        "{kind}: a value-based rectangle on a {width}x{height} stage"
                    );
                }
                for y in 0..height {
                    for x in 0..width {
                        let rgb = pixel::<K>(&mut rng);
                        let coverage = compiled.coverage(x, y, rgb);
                        if coverage != 0.0 {
                            assert!(
                                bounds.contains(x, y),
                                "{kind}, {width}x{height}: coverage {coverage} at ({x}, {y}) lies \
                                 outside {bounds:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    println!("{kind}: {narrower} of {cases} rectangles were smaller than their stage");
    assert!(
        narrower >= K::NARROWER,
        "{kind}: only {narrower} of {cases} rectangles were narrower than the whole stage, against \
         a recorded floor of {}; the property would be holding vacuously",
        K::NARROWER
    );
}

/// The rectangle's closed-form answers: a shape drawn entirely off the frame selects nothing and
/// bounds to nothing; an inverted drawn shape is non-zero at every corner and bounds to the whole
/// stage, and is exactly zero where the shape was full; a value-based component bounds the whole
/// stage at any size; and an amount of exactly zero is empty whatever the components read,
/// inverted or not, because the final multiply is then exactly `0.0`.
fn the_rectangle_is_empty_or_whole_where_it_must_be<K: Kind>() {
    let kind = K::KIND;
    let size = (40u32, 30u32);
    let mut strokes = StrokeTable::new("the kind-conformance suite");
    let nothing = K::nothing(&mut strokes);
    assert_eq!(
        nothing.is_empty(),
        K::VALUE_BASED,
        "{kind}: a position-only kind names a shape that selects nothing, and a value-based one \
         has no position to put off the frame"
    );
    for payload in nothing {
        let compiled = compile(&one_component(kind, payload.clone()), size, &strokes);
        assert!(
            compiled.bounds().is_empty(),
            "{kind}: {payload} bounded something"
        );
        for y in 0..size.1 {
            for x in 0..size.0 {
                assert_eq!(
                    compiled.coverage(x, y, ANY_PIXEL),
                    0.0,
                    "{kind}: {payload} at ({x}, {y})"
                );
            }
        }
    }
    if let Some((payload, (inside_x, inside_y))) = K::inverted_whole(&mut strokes) {
        let mut inverted = one_component(kind, payload);
        inverted.components[0].invert = true;
        let compiled = compile(&inverted, size, &strokes);
        let bounds = compiled.bounds();
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.width, bounds.height),
            (0, 0, size.0, size.1),
            "{kind}: the complement of a bounded shape is not a rectangle"
        );
        for (x, y) in [
            (0, 0),
            (size.0 - 1, 0),
            (0, size.1 - 1),
            (size.0 - 1, size.1 - 1),
        ] {
            assert_eq!(
                compiled.coverage(x, y, ANY_PIXEL),
                1.0,
                "{kind} at ({x}, {y})"
            );
        }
        assert_eq!(
            compiled.coverage(inside_x, inside_y, ANY_PIXEL),
            0.0,
            "{kind}: the inversion inside the shape"
        );
    }
    if K::VALUE_BASED {
        for (width, height) in [(37u32, 29u32), (640, 480), (6000, 4000)] {
            let compiled = compile(
                &one_component(kind, K::feathered(&mut strokes)),
                (width, height),
                &strokes,
            );
            let bounds = compiled.bounds();
            assert_eq!(
                (bounds.x0, bounds.y0, bounds.width, bounds.height),
                (0, 0, width, height),
                "{kind} on a {width}x{height} stage"
            );
        }
    }
    let mut silent = one_component(kind, K::feathered(&mut strokes));
    silent.amount = 0.0;
    for invert in [false, true] {
        silent.invert = invert;
        let compiled = compile(&silent, size, &strokes);
        assert!(
            compiled.bounds().is_empty(),
            "{kind}: amount 0, invert {invert}"
        );
        for (x, y) in [(0u32, 0u32), (20, 15), (39, 29)] {
            for rgb in [ANY_PIXEL, [0.3, 0.4, 0.5], [1.5, -0.2, 0.9]] {
                assert_eq!(compiled.coverage(x, y, rgb), 0.0, "{kind} at ({x}, {y})");
            }
        }
    }
}

/// Every message a malformed or illegal payload of the kind produces, in full, naming the component
/// and the field; and the edges of legality compile.
fn refusals_name_what_they_refuse<K: Kind>() {
    let kind = K::KIND;
    for (payload, error_kind, message) in K::refusals() {
        let error = CompiledMask::new(
            &one_component(kind, payload.clone()),
            stage(400, 400),
            &StrokeTable::default(),
        )
        .expect_err("an illegal payload is refused");
        assert_eq!(error.kind, error_kind, "{kind}: {payload}");
        assert_eq!(error.to_string(), message, "{kind}: {payload}");
    }
    for payload in K::legal() {
        CompiledMask::new(
            &one_component(kind, payload.clone()),
            stage(400, 400),
            &StrokeTable::default(),
        )
        .unwrap_or_else(|error| panic!("{kind}: {payload} was refused: {error}"));
    }
}

/// `min_feature_px` is the ramp's width in the pixels of the stage it is asked about — a stored
/// geometry is normalized, so a proxy stage has that many fewer pixels of it — and where the adapter
/// counts one, the number of rows the transition actually occupies: counted, not argued. A mask's
/// is its narrowest component's.
fn min_feature_px_is_the_measured_ramp<K: Kind>() {
    let kind = K::KIND;
    let mut strokes = StrokeTable::new("the kind-conformance suite");
    let ramps = K::ramps(&mut strokes);
    assert!(!ramps.is_empty(), "{kind} states no ramp");
    for ramp in &ramps {
        let compiled = compile(
            &one_component(kind, ramp.payload.clone()),
            ramp.compiled,
            &strokes,
        );
        let feature = compiled.min_feature_px(stage(ramp.asked.0, ramp.asked.1));
        let what = format!(
            "{kind}: {} compiled at {:?}, asked at {:?}, answered {feature}",
            ramp.payload, ramp.compiled, ramp.asked
        );
        match ramp.expect {
            Expect::Exact(expected) => assert_eq!(feature, expected, "{what}"),
            Expect::Near(expected, tolerance) => {
                assert!(
                    (f64::from(feature) - expected).abs() < tolerance,
                    "{what}, not {expected}"
                );
            }
            Expect::Below(bound) => assert!(feature < bound, "{what}, not below {bound}"),
            Expect::Positive => assert!(feature > 0.0 && feature.is_finite(), "{what}"),
            Expect::Infinite => assert_eq!(feature, f32::INFINITY, "{what}"),
        }
        if let Some((column, rows)) = &ramp.counted {
            let partial = (0..ramp.compiled.1)
                .filter(|y| {
                    let c = compiled.coverage(*column, *y, ANY_PIXEL);
                    c > 0.0 && c < 1.0
                })
                .count();
            assert!(
                rows.contains(&partial),
                "{what}: the ramp occupied {partial} rows against a stated {rows:?}"
            );
        }
    }
    // Two ramps compiled and asked alike, as two components of one mask: the narrower one decides.
    for (index, first) in ramps.iter().enumerate() {
        for second in &ramps[index + 1..] {
            if (first.compiled, first.asked) != (second.compiled, second.asked) {
                continue;
            }
            let asked = stage(first.asked.0, first.asked.1);
            let alone = |payload: &Value| {
                compile(
                    &one_component(kind, payload.clone()),
                    first.compiled,
                    &strokes,
                )
                .min_feature_px(asked)
            };
            let mut both = one_component(kind, first.payload.clone());
            let name = both.next_component_name(kind);
            both.components.push(Component::new(
                name,
                ComponentMode::Add,
                kind,
                second.payload.clone(),
            ));
            assert_eq!(
                compile(&both, first.compiled, &strokes).min_feature_px(asked),
                alone(&first.payload).min(alone(&second.payload)),
                "{kind}: {} beside {}",
                first.payload,
                second.payload
            );
        }
    }
}

/// **Proposal P12's condition.** A position-only kind ignores the pixel it is handed *exactly* —
/// the same `f64` bits for very different pixels at every pixel of a stage, with both inversions
/// and an amount applied — because the mask study compares those kinds bit for bit and a value
/// leaking into a falloff would break that silently. A value-based kind is the other way round: it
/// ignores the position exactly. The kind table's `value_based` column and a compiled component's
/// own `reads_pixels` agree.
fn reads_exactly_what_its_kind_says<K: Kind>() {
    let kind = K::KIND;
    let size = (37u32, 29u32);
    let mut strokes = StrokeTable::new("the kind-conformance suite");
    let mut mask = one_component(kind, K::feathered(&mut strokes));
    mask.components[0].invert = true;
    mask.invert = true;
    mask.amount = 62.5;
    let compiled = compile(&mask, size, &strokes);
    assert_eq!(
        compiled.reads_pixels(),
        K::VALUE_BASED,
        "{kind}: the kind table and the compiled component disagree about reading the pixel"
    );
    let pixels = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.2, 0.7, 0.35],
        [-0.4, 2.3, 0.9],
        [f64::MIN_POSITIVE, 0.5, 1.7],
    ];
    if K::VALUE_BASED {
        for rgb in pixels {
            let first = compiled.coverage(0, 0, rgb).to_bits();
            for y in 0..size.1 {
                for x in 0..size.0 {
                    assert_eq!(
                        compiled.coverage(x, y, rgb).to_bits(),
                        first,
                        "{kind}: {rgb:?} moved at ({x}, {y})"
                    );
                }
            }
        }
    } else {
        for y in 0..size.1 {
            for x in 0..size.0 {
                let first = compiled.coverage(x, y, pixels[0]).to_bits();
                for rgb in &pixels[1..] {
                    assert_eq!(
                        compiled.coverage(x, y, *rgb).to_bits(),
                        first,
                        "{kind} at ({x}, {y}) moved when handed {rgb:?}"
                    );
                }
                // The narrowed `f32` answers the same way, which is the call a render makes.
                assert_eq!(
                    compiled.evaluate(x, y, [0.0, 0.0, 0.0]).to_bits(),
                    compiled.evaluate(x, y, [1.0, 0.25, 0.75]).to_bits(),
                    "{kind} at ({x}, {y})"
                );
            }
        }
    }
}

/// **A sampled byte equals the rendered byte** at every pixel of a masked Basic layer over the
/// kind's feathered payload, on the byte path and on the RAW-shaped linear path: `render.sample` and
/// the rasterizing pass reach the mask through one call with the same arguments, the pixel among
/// them. The payload keeps much of the frame on its transition, where a point query that read the
/// wrong pixel or the wrong position would disagree.
fn a_sampled_byte_equals_the_rendered_byte<K: Kind>() {
    let kind = K::KIND;
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let linear = decoded(&source);
    let mut strokes = StrokeTable::new("the kind-conformance suite");
    let mask = one_component(kind, K::feathered(&mut strokes));
    let compiled = compile(&mask, (WIDTH, HEIGHT), &strokes);
    let stack = Recipe {
        strokes,
        ..recipe(
            vec![masked(basic_layer(json!({"exposure": MASKED_EV})), &mask)],
            vec![mask.clone()],
        )
    };
    let rendered = render(&registry, &source, SnapshotId::new(), &stack)
        .unwrap_or_else(|error| panic!("{kind}: {error}"));
    let rendered_linear = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &stack,
        LinearSettings::default(),
    )
    .unwrap_or_else(|error| panic!("{kind}: {error}"));
    let mut partial = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = ((y * WIDTH + x) * 4) as usize;
            let input: [f64; 3] = std::array::from_fn(|channel| {
                luxforge_reference::srgb::decode(source.rgba[offset + channel])
            });
            let m = compiled.coverage(x, y, input);
            if m > 0.0 && m < 1.0 {
                partial += 1;
            }
            assert_eq!(
                sample(&registry, &source, &stack, x, y).unwrap().rgba,
                rendered.pixel(x, y),
                "{kind}: the byte path's sample differs from its render at ({x}, {y})"
            );
            assert_eq!(
                sample_linear(&registry, &linear, &stack, LinearSettings::default(), x, y)
                    .unwrap()
                    .rgba,
                rendered_linear.pixel(x, y),
                "{kind}: the linear path's sample differs from its render at ({x}, {y})"
            );
        }
    }
    assert!(
        partial * 4 >= (WIDTH * HEIGHT) as usize,
        "{kind}: only {partial} of {} pixels are on the transition",
        WIDTH * HEIGHT
    );
}

// ---------------------------------------------------------------------------
// Beside the rows
// ---------------------------------------------------------------------------

/// The suite covers the kind table exactly, in its order: a kind registered without an adapter
/// fails here rather than going unchecked.
#[test]
fn every_registered_kind_passes_the_checklist() {
    assert_eq!(
        mask::component_kinds().collect::<Vec<_>>(),
        adapters().map(|(kind, _)| kind),
        "every registered kind has an adapter, in the table's order"
    );
}

/// Components of every kind in one mask, composed together, still agree bit for bit with the fold
/// over their references: the kinds meet only through the fold, and the fold is the frozen one.
#[test]
fn kinds_meet_only_through_the_fold() {
    let adapters = adapters();
    let mut rng = SplitMix64(0x4A5C_0014);
    let mut met = std::collections::BTreeMap::<&str, usize>::new();
    for components in 2..=4 {
        for round in 0..30 {
            let sampled = sample_mask(&mut rng, components, |rng, strokes| {
                let (kind, sample) = adapters[rng.next_usize(adapters.len())];
                let (payload, falloff) = sample(rng, &SMALL, strokes);
                (kind, payload, falloff)
            });
            for kind in &sampled.kinds {
                *met.entry(kind).or_default() += 1;
            }
            for (width, height) in SMALL {
                let compiled = compile(&sampled.mask, (width, height), &sampled.strokes);
                let reference_stage = ref_stage(width, height);
                for y in 0..height {
                    for x in 0..width {
                        let rgb = sample_pixel(&mut rng);
                        assert_eq!(
                            compiled.coverage(x, y, rgb).to_bits(),
                            sampled.fold.coverage(&reference_stage, x, y, rgb).to_bits(),
                            "{:?}, round {round}, {width}x{height} at ({x}, {y})",
                            sampled.kinds
                        );
                    }
                }
            }
        }
    }
    for (kind, _) in adapters {
        assert!(
            met.get(kind).copied().unwrap_or(0) >= 10,
            "{kind} met the other kinds only {met:?}"
        );
    }
}

/// A component of a kind this build does not claim is refused by name as `incompatible` — by the
/// compile and by the stage-free check alike — and the stored mask reads back with its bytes
/// intact, which is what retention means. The table answers the other questions about it without
/// inventing anything: no glyph, and its title as its menu title.
#[test]
fn an_unknown_kind_is_refused_by_name_and_still_reads_back() {
    // Every kind the masking design names is delivered, so the kind this build does not claim is one
    // no design names — which is exactly the case retention exists for.
    assert!(!mask::knows_component_kind("depth-range"));
    assert_eq!(mask::kind_icon("depth-range"), None);
    assert_eq!(mask::kind_menu_title("cloud"), "Cloud");
    let mut stored = Mask::new("Mask 1");
    let name = stored.next_component_name("depth-range");
    stored.components.push(Component::new(
        name,
        ComponentMode::Add,
        "depth-range",
        json!({"low": 0.2, "low_feather": 10.0, "nested": {"points": [[0.25, 0.5]]}, "flag": true}),
    ));
    let error = CompiledMask::new(&stored, stage(64, 48), &StrokeTable::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert_eq!(
        error.to_string(),
        "incompatible: unknown mask component depth-range"
    );
    assert_eq!(
        mask::validate_component_kinds(&stored)
            .unwrap_err()
            .to_string(),
        "incompatible: unknown mask component depth-range"
    );
    // The stored mask is structurally valid — the model never asks what a kind means — and survives
    // a round trip byte for byte, untouched by the refusal: the table parses, it does not rewrite.
    stored.validate().unwrap();
    assert_eq!(stored.components[0].kind, "depth-range");
    assert_eq!(stored.components[0].payload["flag"], json!(true));
    let encoded = serde_json::to_vec(&stored).unwrap();
    let reopened: Mask = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(reopened, stored);
    assert_eq!(
        serde_json::to_string(&reopened.components[0].payload).unwrap(),
        serde_json::to_string(&stored.components[0].payload).unwrap()
    );
}
