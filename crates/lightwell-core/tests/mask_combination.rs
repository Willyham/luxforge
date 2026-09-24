//! TASK-013: the combination vocabulary, proved where it matters — in **pixels**.
//!
//! Two questions this file answers that no earlier mask test could:
//!
//! 1. A mask holding components of *several kinds*, in every mode, with inversions at both levels,
//!    a whole-mask amount and a whole-mask inversion, **renders** exactly as the independent `f64`
//!    reference composes it. `mask_unit.rs` and `mask_radial.rs` compare the coverage *field*
//!    against that reference; this compares the frame a person would see, over randomized component
//!    lists, through the same public render path a client reaches.
//! 2. A **radial gradient** goes end to end: created, added to as a second kind, patched on a
//!    radius, an angle and a feather, committed and rendered, through the generated `mask.*` methods
//!    and the delivered `edit.set-basic` target. That is the gap the generated geometry methods
//!    close — until they existed the radial was evaluable and not creatable — and it is proved with
//!    bytes rather than with a stack listing.
//!
//! The oracle is `tests/reference/mask.rs`, which shares no code with `lightwell-core`, composed
//! with `tests/reference/mod.rs`'s sRGB and exposure reference and that module's own `blend`,
//! `out = (1 − M)·in + M·effect(in)`. The numerical rule is the delivered colour studies': a
//! rendered code equals the reference's code exactly, except where the reference's linear value sits
//! within `1e-6 + 1e-6·|threshold|` of a code threshold, where one code is permitted because
//! production decodes, multiplies and blends in `f32`.

mod reference;

use lightwell_core::{
    AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EditorService, Layer, LayerId,
    Mask, MaskId, ModuleRegistry, Mutation, RECIPE_FORMAT, Raster, Recipe, SnapshotId, SourceImage,
    mask::commands::{self, MaskTarget},
    path::Stroke,
    render,
};
use reference::mask::{
    Algebra, Brush, BrushStroke, Component as RefComponent, Kind, Linear, Mask as RefMask, Mode,
    Radial, Stage as RefStage, axis_is_legal, blend, brush_coverage, combine, coverage,
};
use reference::{code_threshold, linear_to_srgb_code, srgb_to_linear};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The pixel value a geometric component is handed and ignores (proposal P12 of
/// `docs/design/range-study.md`). These masks hold gradients and brushes, whose coverage is a
/// function of position alone, so the value here is arbitrary and the same at every call;
/// `mask_range.rs` proves that ignoring it is exact rather than approximate.
const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];

// ---------------------------------------------------------------------------------------------
// Randomized mixed-kind composition, rendered
// ---------------------------------------------------------------------------------------------

const WIDTH: u32 = 24;
const HEIGHT: u32 = 16;
/// The exposure the masked layer applies, in EV. Large enough that a coverage difference of one part
/// in a thousand is visible in the output codes rather than lost in the quantizer.
const MASKED_EV: f64 = 2.0;

/// SplitMix64, the same dependency-free generator the mask study's own figures use, so every sweep
/// below is reproducible on any machine from its stated seed.
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

    fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

/// A source whose bytes vary on both axes and in all three channels, so a wrong row, a dropped
/// channel or a mask read at the wrong coordinate is visible in the comparison.
fn byte_source() -> SourceImage {
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgba.extend([
                (x * 9 + 3) as u8,
                (y * 13 + 40) as u8,
                (x * 5 + y * 7 + 90) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width: WIDTH,
        height: HEIGHT,
        rgba: rgba.into(),
        fingerprint: "sha256:mask-combination-fixture".into(),
        orientation: 1,
    }
}

fn mode_of(index: usize) -> (ComponentMode, Mode) {
    match index {
        0 => (ComponentMode::Add, Mode::Add),
        1 => (ComponentMode::Subtract, Mode::Subtract),
        _ => (ComponentMode::Intersect, Mode::Intersect),
    }
}

/// One randomized linear gradient whose axis is legal on the fixture's stage.
fn sample_linear(rng: &mut SplitMix64) -> Linear {
    loop {
        let linear = Linear {
            x0: rng.next_range(-0.2, 1.2),
            y0: rng.next_range(-0.2, 1.2),
            x1: rng.next_range(-0.2, 1.2),
            y1: rng.next_range(-0.2, 1.2),
        };
        if axis_is_legal(&linear, &RefStage::new(WIDTH, HEIGHT)) {
            return linear;
        }
    }
}

/// One randomized radial gradient inside every legality rule the study states.
fn sample_radial(rng: &mut SplitMix64) -> Radial {
    Radial {
        x: rng.next_range(-0.2, 1.2),
        y: rng.next_range(-0.2, 1.2),
        radius_x: rng.next_range(0.05, 1.2),
        radius_y: rng.next_range(0.05, 1.2),
        angle: rng.next_range(-180.0, 180.0),
        feather: rng.next_range(0.0, 100.0),
    }
}

fn linear_payload(linear: &Linear) -> Value {
    json!({"x0": linear.x0, "y0": linear.y0, "x1": linear.x1, "y1": linear.y1})
}

fn radial_payload(radial: &Radial) -> Value {
    json!({
        "x": radial.x, "y": radial.y,
        "radius_x": radial.radius_x, "radius_y": radial.radius_y,
        "angle": radial.angle, "feather": radial.feather,
    })
}

/// One randomized **mixed-kind** mask in both spellings: the stored model the host compiles, and the
/// reference's own structure, built side by side from the same numbers so the comparison is a
/// comparison of implementations rather than of payloads.
///
/// The first component is always `add`, which the model validates structurally; every later one
/// takes a random kind, a random mode and a random inversion, and the whole mask takes a random
/// amount and a random inversion.
fn sample_pair(rng: &mut SplitMix64, components: usize) -> (Mask, RefMask) {
    let mut mask = Mask::new("Mask 1");
    mask.amount = rng.next_range(0.0, 100.0);
    mask.invert = rng.next_bool();
    let mut reference = RefMask {
        amount: mask.amount,
        invert: mask.invert,
        components: Vec::new(),
    };
    for index in 0..components {
        let (mode, ref_mode) = if index == 0 {
            (ComponentMode::Add, Mode::Add)
        } else {
            mode_of(rng.next_usize(3))
        };
        let invert = rng.next_bool();
        let (kind, payload, ref_kind) = if rng.next_bool() {
            let linear = sample_linear(rng);
            ("linear", linear_payload(&linear), Kind::Linear(linear))
        } else {
            let radial = sample_radial(rng);
            ("radial", radial_payload(&radial), Kind::Radial(radial))
        };
        let name = mask.next_component_name(kind);
        let mut component = Component::new(name, mode, kind, payload);
        component.invert = invert;
        mask.components.push(component);
        reference.components.push(RefComponent {
            mode: ref_mode,
            invert,
            kind: ref_kind,
        });
    }
    (mask, reference)
}

/// The colour-study tolerance: an exact code everywhere except within `1e-6 + 1e-6·|threshold|` of a
/// code boundary, where one code of difference is allowed.
fn assert_code(actual: u8, expected: u8, linear: f64, case: &str) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(
        difference.abs() <= 1,
        "{case}: rendered {actual} against reference {expected}"
    );
    let crossed = actual.max(expected);
    assert!(crossed >= 1, "{case}: code 0 has no lower threshold");
    let threshold = code_threshold(crossed);
    let tolerance = 1e-6 + 1e-6 * threshold.abs();
    assert!(
        (linear - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected} is not within {tolerance} of the threshold \
         {threshold} (reference linear {linear})"
    );
}

/// A mask of several components of different kinds renders exactly as the reference composes them,
/// over randomized component lists: every mode, inversion at both levels, and a whole-mask amount,
/// on every pixel of the fixture.
///
/// The frame is compared and not the field, so this covers the composition, the compiled mask, the
/// bounds rectangle a masked colour run skips spans with, and the blend — a rectangle that wrongly
/// excluded a covered span would show here as an unblended pixel and nowhere in a coverage test.
#[test]
fn a_mixed_kind_mask_renders_exactly_as_the_reference_composes_it() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let reference_stage = RefStage::new(WIDTH, HEIGHT);
    let mut rng = SplitMix64(0x4A5C_0013);
    let mut kinds_seen = (false, false);
    let mut partial_total = 0usize;
    let mut checked = 0usize;
    for components in 1..=5 {
        for round in 0..12 {
            let (mask, oracle) = sample_pair(&mut rng, components);
            mask.validate().expect("the sampled mask is structural");
            for component in &mask.components {
                match component.kind.as_str() {
                    "linear" => kinds_seen.0 = true,
                    "radial" => kinds_seen.1 = true,
                    other => panic!("unexpected kind {other}"),
                }
            }
            let stack = Recipe {
                format: RECIPE_FORMAT,
                layers: vec![Layer {
                    id: LayerId::new(),
                    effect_id: BASIC_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload: json!({"exposure": MASKED_EV}),
                    mask: Some(mask.id.clone()),
                    artifacts: Vec::new(),
                }],
                masks: vec![mask.clone()],
                ..Recipe::default()
            };
            let rendered = render(&registry, &source, SnapshotId::new(), &stack)
                .unwrap_or_else(|error| panic!("{components} components, round {round}: {error}"));
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    let m = coverage(&oracle, Algebra::Zadeh, &reference_stage, u, v);
                    if m > 0.0 && m < 1.0 {
                        partial_total += 1;
                    }
                    let offset = ((y * WIDTH + x) * 4) as usize;
                    let pixel = rendered.pixel(x, y).expect("a pixel of the stage");
                    for (channel, &code) in pixel.iter().enumerate().take(3) {
                        let input = srgb_to_linear(source.rgba[offset + channel]);
                        let effect = input * 2.0_f64.powf(MASKED_EV);
                        let blended = blend(input, effect, m);
                        assert_code(
                            code,
                            linear_to_srgb_code(blended),
                            blended,
                            &format!(
                                "{components} components, round {round}, ({x}, {y}) channel \
                                 {channel}"
                            ),
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(
        kinds_seen.0 && kinds_seen.1,
        "the sweep must actually mix kinds: {kinds_seen:?}"
    );
    assert!(
        partial_total > 1000,
        "only {partial_total} partially covered samples: the sweep is all endpoints"
    );
    assert_eq!(checked, 5 * 12 * (WIDTH * HEIGHT * 3) as usize);
    println!(
        "{checked} rendered channels over mixed-kind masks, {partial_total} of them partially \
         covered, all within the colour-study tolerance of the reference"
    );
}

/// `invert` is applied to the composed coverage and `amount` multiplies the result — in that order,
/// which is the study's algebra and not a choice this unit gets to make. The two differ wherever
/// coverage is not 0 or 1 and the amount is not 100, so the test is a real discrimination and not a
/// restatement: the rejected order would render `amount·m` inverted, which is a different frame.
#[test]
fn invert_applies_before_amount_and_the_reference_agrees() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let reference_stage = RefStage::new(WIDTH, HEIGHT);
    // A vertical gradient over the middle half of the frame, so the top row sits exactly behind `p0`
    // and the bottom row exactly beyond `p1` and the two ends are exact rather than nearly so.
    let gradient = Linear {
        x0: 0.5,
        y0: 0.25,
        x1: 0.5,
        y1: 0.75,
    };
    let mut mask = Mask::new("Mask 1");
    mask.amount = 40.0;
    mask.invert = true;
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        linear_payload(&gradient),
    ));
    let oracle = RefMask {
        amount: 40.0,
        invert: true,
        components: vec![RefComponent {
            mode: Mode::Add,
            invert: false,
            kind: Kind::Linear(gradient),
        }],
    };
    let stack = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": MASKED_EV}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        }],
        masks: vec![mask.clone()],
        ..Recipe::default()
    };
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let mut differed = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (u, v) = reference_stage.pixel_uv(x, y);
            let m = coverage(&oracle, Algebra::Zadeh, &reference_stage, u, v);
            // The order the study rejected, written out so the two can be told apart.
            let raw = coverage(
                &RefMask {
                    amount: 100.0,
                    invert: false,
                    ..oracle.clone()
                },
                Algebra::Zadeh,
                &reference_stage,
                u,
                v,
            );
            let wrong_order = 1.0 - (oracle.amount / 100.0) * raw;
            if (m - wrong_order).abs() > 1e-12 {
                differed += 1;
            }
            let offset = ((y * WIDTH + x) * 4) as usize;
            let pixel = rendered.pixel(x, y).expect("a pixel of the stage");
            for (channel, &code) in pixel.iter().enumerate().take(3) {
                let input = srgb_to_linear(source.rgba[offset + channel]);
                let effect = input * 2.0_f64.powf(MASKED_EV);
                let blended = blend(input, effect, m);
                assert_code(
                    code,
                    linear_to_srgb_code(blended),
                    blended,
                    &format!("({x}, {y}) channel {channel}"),
                );
            }
        }
    }
    assert_eq!(
        differed,
        (WIDTH * HEIGHT) as usize,
        "every pixel must tell the two orders apart, or this proves nothing"
    );
    // And the frozen order at the two ends, in closed form: behind p0 the component is 0, so the
    // inversion makes it 1 and the amount scales it to 0.4; beyond p1 it is 1, inverted to 0.
    let compiled = lightwell_core::mask::CompiledMask::new(
        &mask,
        lightwell_core::Stage {
            width: WIDTH,
            height: HEIGHT,
        },
        &lightwell_core::path::StrokeTable::default(),
    )
    .unwrap();
    assert_eq!(compiled.coverage(WIDTH / 2, 0, ANY_PIXEL), 0.4);
    assert_eq!(compiled.coverage(WIDTH / 2, HEIGHT - 1, ANY_PIXEL), 0.0);
}

// ---------------------------------------------------------------------------------------------
// The radial, end to end, in pixels
// ---------------------------------------------------------------------------------------------

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightwell-mask-combination-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

fn luma(raster: &Raster, x: u32, y: u32) -> f64 {
    let p = raster.pixel(x, y).expect("pixel inside the stage");
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

/// The masked edit these journeys make. It **darkens**, deliberately: the fixture's middle is a
/// clipped white sky, so lifting it would change nothing there and a test that claimed otherwise
/// would be reading the clamp rather than the mask. Every pixel of the fixture is above black, so a
/// covered pixel always moves.
const MASKED_EXPOSURE: f64 = -2.0;

/// A covered pixel is visibly darker than the same pixel of the unmasked frame.
fn assert_darkened(masked: &Raster, unmasked: &Raster, x: u32, y: u32, case: &str) {
    let (after, before) = (luma(masked, x, y), luma(unmasked, x, y));
    assert!(
        after < before - 1.0,
        "{case}: ({x}, {y}) is {after} against an unmasked {before}, which is not darker"
    );
}

struct Fixture {
    service: EditorService,
    asset: AssetId,
}

impl Fixture {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        Self { service, asset }
    }

    fn mutation(&self, request: &str) -> Mutation {
        Mutation {
            expected_revision: self.service.state(&self.asset).unwrap().revision,
            request_id: request.to_owned(),
            actor: "mask-combination".to_owned(),
        }
    }

    /// One `mask.*` command, resolved through the same table `schema.list` publishes.
    fn mask_command(
        &mut self,
        method: &str,
        target: MaskTarget,
        parameters: Value,
        request: &str,
    ) -> lightwell_core::mask::commands::MaskCommandResult {
        let command =
            commands::find(method).unwrap_or_else(|| panic!("{method} is a declared command"));
        let mutation = self.mutation(request);
        self.service
            .apply_mask_command(&self.asset, mutation, command, parameters, target)
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    fn edit(&mut self, action: &str, parameters: Value, request: &str) {
        let mutation = self.mutation(request);
        self.service
            .apply_action(&self.asset, mutation, action, parameters)
            .unwrap_or_else(|e| panic!("{action} failed: {e}"));
    }

    fn component_of(&self, mask: &MaskId, name: &str) -> MaskTarget {
        let entry = self.service.state(&self.asset).unwrap().current_entry.id;
        let listing = self.service.mask_listing(&self.asset, &entry).unwrap();
        let found = listing
            .masks
            .iter()
            .find(|report| &report.id == mask)
            .expect("the mask is listed");
        let component = found
            .components
            .iter()
            .find(|component| component.name == name)
            .unwrap_or_else(|| panic!("no component named {name}"));
        MaskTarget {
            mask: Some(mask.clone()),
            component: Some(component.id.clone()),
            ..MaskTarget::default()
        }
    }
}

/// The end-to-end gap the generated geometry methods close: a radial gradient is **created**, a
/// linear is **added** to the same mask as a second kind, the radial is **patched** on a radius, an
/// angle and a feather, and every step is **committed and rendered**.
///
/// It is proved in bytes. Until `mask.create-radial` existed a radial could be evaluated but never
/// reached from a client at all, so a listing that named one would have proved nothing about the
/// picture.
#[test]
fn a_radial_is_created_added_patched_and_rendered_end_to_end() {
    let mut f = Fixture::open("radial");
    let unmasked = f.service.render_current(&f.asset).unwrap();
    let (width, height) = (unmasked.width, unmasked.height);
    let (cx, cy) = (width / 2, height / 2);

    // An ellipse at the centre of the frame, a fifth of the stage's height across, with a hard edge.
    let created = f.mask_command(
        "mask.create-radial",
        MaskTarget::default(),
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.2, "radius_y": 0.2, "angle": 0.0, "feather": 0.0}),
        "create",
    );
    let mask = created.mask.clone().expect("the create names its mask");
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": MASKED_EXPOSURE}),
        "darken",
    );

    let masked = f.service.render_current(&f.asset).unwrap();
    assert_eq!(
        (masked.width, masked.height),
        (width, height),
        "a mask changes no dimension"
    );
    // Inside the ellipse is selected, which is proposal P3's recorded default.
    assert_darkened(&masked, &unmasked, cx, cy, "the centre of the ellipse");
    // The first row is entirely outside an ellipse of radius 0.2·H centred on the frame, so the
    // bounds rectangle skips it and it must be byte-identical to the unmasked render.
    let row = width as usize * 4;
    assert_eq!(
        &masked.rgba[..row],
        &unmasked.rgba[..row],
        "the uncovered top row must be untouched, byte for byte"
    );
    assert_eq!(
        masked.pixel(0, cy),
        unmasked.pixel(0, cy),
        "and so must a pixel beside the ellipse on its own row"
    );

    // A second component of a *different* kind, subtracting the right half of the frame.
    f.mask_command(
        "mask.add-linear",
        MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        },
        json!({"mode": "subtract", "x0": 0.45, "y0": 0.5, "x1": 0.55, "y1": 0.5}),
        "add-linear",
    );
    let combined = f.service.render_current(&f.asset).unwrap();
    let right = cx + width / 8;
    assert_eq!(
        combined.pixel(right, cy),
        unmasked.pixel(right, cy),
        "a subtract component removes the coverage the radial placed on the right of the ellipse"
    );
    let left = cx - width / 8;
    assert_darkened(
        &combined,
        &unmasked,
        left,
        cy,
        "the left of the ellipse, which the subtract does not reach",
    );

    // Patch the radial on the three fields no position range could ever have carried: a radius, an
    // angle and a feather. The ellipse grows and tilts, and a pixel that was outside it is now in.
    let radial = f.component_of(&mask, "Radial 1");
    // A sample pixel on the centre row, between a quarter and a sixth of the frame's width to the
    // left of the centre: further from the centre than the ellipse's 0.2 radius reaches, and inside
    // the grown one. It is chosen rather than named because the fixture has black pixels, and a
    // black pixel cannot be darkened — a test standing on one would assert nothing.
    let outside = ((cx - width / 4)..(cx - width / 6))
        .map(|x| (x, cy))
        .find(|&(x, y)| {
            combined.pixel(x, y) == unmasked.pixel(x, y) && luma(&unmasked, x, y) > 40.0
        })
        .expect("a pixel outside the ellipse with room to darken");
    assert_eq!(
        combined.pixel(outside.0, outside.1),
        unmasked.pixel(outside.0, outside.1),
        "the sample pixel starts outside the ellipse"
    );
    let patched = f.mask_command(
        "mask.set-radial",
        radial,
        json!({"radius_x": 0.9, "radius_y": 0.35, "angle": -25.0, "feather": 60.0}),
        "patch",
    );
    assert_eq!(patched.label.as_deref(), Some("Update Radial 1"));
    let grown = f.service.render_current(&f.asset).unwrap();
    assert_darkened(
        &grown,
        &unmasked,
        outside.0,
        outside.1,
        "a radius, an angle and a feather are editable after the fact and the picture follows",
    );

    // `render.sample` equals the rendered byte inside the feather band, at a bounds edge and
    // outside it — the same coverage call reached two ways.
    let entry = f.service.state(&f.asset).unwrap().current_entry.id;
    for (x, y) in [
        (cx, cy),
        (outside.0, outside.1),
        (left, cy),
        (right, cy),
        (0, 0),
        (width - 1, height - 1),
    ] {
        let sample = f.service.sample_entry(&f.asset, &entry, x, y).unwrap();
        assert_eq!(
            sample.rgba,
            grown.pixel(x, y).expect("pixel inside the stage"),
            "render.sample must equal the rendered byte at ({x}, {y})"
        );
    }

    // One entry per command, each with the label the design's granularity table states.
    let labels: Vec<String> = f
        .service
        .history(&f.asset, None, 100)
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.label.clone())
        .rev()
        .filter(|label| label != "Original")
        .collect();
    assert_eq!(
        labels,
        [
            "Add radial",
            "Mask 1 · Exposure -2.00 EV",
            "Add subtract linear",
            "Update Radial 1",
        ],
        "each command is one entry with a readable label"
    );
}

/// A component's mode and inversion are properties you change **after** the fact, each one command,
/// one history entry and one readable label — and each changes the picture. That is the owner's
/// complaint about Lightroom answered in pixels: there, the mode of a component is decided by which
/// button created it and cannot be revisited.
#[test]
fn a_components_mode_and_inversion_are_editable_after_it_exists() {
    let mut f = Fixture::open("modes");
    let unmasked = f.service.render_current(&f.asset).unwrap();
    let width = unmasked.width;
    let (cx, cy) = (width / 2, unmasked.height / 2);

    let created = f.mask_command(
        "mask.create-radial",
        MaskTarget::default(),
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.3, "angle": 0.0, "feather": 0.0}),
        "create",
    );
    let mask = created.mask.clone().expect("the create names its mask");
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": MASKED_EXPOSURE}),
        "darken",
    );
    let inside = f.service.render_current(&f.asset).unwrap();
    assert_darkened(&inside, &unmasked, cx, cy, "inside a radial is selected");
    assert_eq!(
        inside.pixel(0, 0),
        unmasked.pixel(0, 0),
        "and outside it is untouched, byte for byte"
    );

    // Inverting the component alone turns the selection inside out, exactly: the centre returns to
    // the unmasked byte and the corner moves.
    let radial = f.component_of(&mask, "Radial 1");
    let inverted = f.mask_command(
        "mask.set-component-invert",
        radial,
        json!({"invert": true}),
        "invert-component",
    );
    assert_eq!(inverted.label.as_deref(), Some("Radial 1 inverted"));
    let outside = f.service.render_current(&f.asset).unwrap();
    assert_eq!(
        outside.pixel(cx, cy),
        unmasked.pixel(cx, cy),
        "an inverted radial leaves its inside untouched, byte for byte"
    );
    assert_darkened(
        &outside,
        &unmasked,
        0,
        0,
        "an inverted radial selects outside",
    );

    // A second component of another kind, added as `add`, then promoted to `intersect` after the
    // fact. Its ramp runs from x = 0.1 to x = 0.3, so it is exactly zero at the left edge and
    // exactly one at the right.
    f.mask_command(
        "mask.add-linear",
        MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        },
        json!({"mode": "add", "x0": 0.1, "y0": 0.5, "x1": 0.3, "y1": 0.5}),
        "add-linear",
    );
    let linear = f.component_of(&mask, "Linear 1");
    let promoted = f.mask_command(
        "mask.set-component-mode",
        linear,
        json!({"mode": "intersect"}),
        "intersect",
    );
    assert_eq!(promoted.label.as_deref(), Some("Linear 1 intersect"));
    let intersected = f.service.render_current(&f.asset).unwrap();
    assert_eq!(
        intersected.pixel(0, 0),
        unmasked.pixel(0, 0),
        "an intersect removes the coverage the left edge had, exactly"
    );
    let right = width - 1;
    assert_darkened(
        &intersected,
        &unmasked,
        right,
        0,
        "and keeps it where both components cover",
    );

    // The whole-mask amount scales the composed result, and it too is one entry with a readable
    // label: at 50 the frame lands strictly between the unmasked one and the fully masked one.
    let halved = f.mask_command(
        "mask.set-amount",
        MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        },
        json!({"amount": 50.0}),
        "amount",
    );
    assert_eq!(halved.label.as_deref(), Some("Amount 50"));
    let scaled = f.service.render_current(&f.asset).unwrap();
    let (before, between, after) = (
        luma(&unmasked, right, 0),
        luma(&scaled, right, 0),
        luma(&intersected, right, 0),
    );
    assert!(
        between < before - 1.0 && between > after + 1.0,
        "an amount of 50 lands between the unmasked {before} and the fully masked {after}, not at \
         {between}"
    );

    // Five commands, five entries, each with the label the design's granularity table states.
    let labels: Vec<String> = f
        .service
        .history(&f.asset, None, 100)
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.label.clone())
        .rev()
        .filter(|label| label != "Original")
        .collect();
    assert_eq!(
        labels,
        [
            "Add radial",
            "Mask 1 · Exposure -2.00 EV",
            "Radial 1 inverted",
            "Add linear",
            "Linear 1 intersect",
            "Amount 50",
        ],
        "a mode change and an inversion are each one command and one readable entry"
    );
}

// ---------------------------------------------------------------------------------------------
// A brush over a gradient, rendered (TASK-021)
// ---------------------------------------------------------------------------------------------

/// The reference's view of a **stored** stroke, which is what production evaluates: the positions
/// snapped to the path grid and decimated there, and the radius quantized to that same grid.
fn as_reference(stroke: &Stroke) -> BrushStroke {
    BrushStroke {
        points: stroke.points().collect(),
        size: stroke.size(),
        feather: stroke.feather(),
        flow: stroke.flow(),
        erase: stroke.erase(),
        colour: None,
    }
}

/// One random stroke, posted the way a client posts one — through the host's own capture — so every
/// stroke below is one a gesture could have produced, on the grid a stored stroke is held on.
fn sample_stroke(rng: &mut SplitMix64, erase: bool) -> Stroke {
    let count = 1 + rng.next_usize(4);
    let mut points = Vec::with_capacity(count);
    let mut x = rng.next_range(0.1, 0.9);
    let mut y = rng.next_range(0.1, 0.9);
    for _ in 0..count {
        points.push([x, y]);
        x = (x + rng.next_range(-0.3, 0.3)).clamp(-1.0, 2.0);
        y = (y + rng.next_range(-0.3, 0.3)).clamp(-1.0, 2.0);
    }
    Stroke::capture(
        &points,
        rng.next_range(0.05, 0.45),
        rng.next_range(0.0, 100.0),
        rng.next_range(20.0, 100.0),
        erase,
    )
    .expect("a legal stroke")
}

/// A brush subtracting from a gradient renders exactly what the frozen algebra and the frozen
/// accumulation rules compose, over randomized strokes and randomized radials, on every pixel.
///
/// This is phase C's claim in its strongest form and the one the owner asked for: *a brush can take
/// a region out of a gradient*. The frame is compared and not the coverage field, so it covers the
/// accumulation inside the component (screen union for an add stroke, multiply-complement for an
/// erase, in stored order), the Zadeh composition between the two components, the compiled brush's
/// grid index, the conservative bounds rectangle a masked colour run skips spans with — a rectangle
/// that wrongly excluded a covered span would show here as an unblended pixel and nowhere in a
/// coverage test — and the blend itself.
///
/// The oracle is `tests/reference/mask.rs`, which shares no code with `lightwell-core`: its
/// `brush_coverage` over the same stored strokes, its `radial_coverage`, and its own `combine`.
#[test]
fn a_brush_subtracting_from_a_gradient_renders_exactly_as_the_reference_composes_it() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let reference_stage = RefStage::new(WIDTH, HEIGHT);
    let mut rng = SplitMix64(0x0BB5_2101);
    let mut partial_total = 0usize;
    let mut erased_total = 0usize;
    let mut checked = 0usize;

    for round in 0..12 {
        // One gradient to take a region out of, and one brush of two to four strokes to take it
        // with — one of them an erase, so the component's own accumulation is exercised and not
        // only its composition.
        let radial = sample_radial(&mut rng);
        let held: Vec<Stroke> = (0..2 + rng.next_usize(3))
            .map(|index| sample_stroke(&mut rng, index == 1))
            .collect();
        let mut table = lightwell_core::path::StrokeTable::new("the brush-over-gradient test");
        let addresses: Vec<String> = held
            .iter()
            .map(|stroke| table.insert(stroke.clone()).to_string())
            .collect();

        let mut mask = Mask::new("Mask 1");
        let radial_name = mask.next_component_name("radial");
        mask.components.push(Component::new(
            radial_name,
            ComponentMode::Add,
            "radial",
            radial_payload(&radial),
        ));
        let brush_name = mask.next_component_name("brush");
        mask.components.push(Component::new(
            brush_name,
            ComponentMode::Subtract,
            "brush",
            json!({ "strokes": addresses }),
        ));
        mask.validate().expect("the sampled mask is structural");

        let stack = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![Layer {
                id: LayerId::new(),
                effect_id: BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": MASKED_EV}),
                mask: Some(mask.id.clone()),
                artifacts: Vec::new(),
            }],
            masks: vec![mask],
            strokes: table,
            artifacts: Default::default(),
        };
        let rendered = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));

        let brush = Brush {
            strokes: held.iter().map(as_reference).collect(),
        };
        erased_total += held.iter().filter(|stroke| stroke.erase()).count();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let (u, v) = reference_stage.pixel_uv(x, y);
                // The composition, in the reference's own spelling: the gradient into an empty
                // mask, then the brush taken out of it.
                let gradient = reference::mask::radial_coverage(&radial, &reference_stage, u, v);
                let painted = brush_coverage(&brush, &reference_stage, u, v, ANY_PIXEL);
                let m = combine(
                    Algebra::Zadeh,
                    combine(Algebra::Zadeh, 0.0, Mode::Add, gradient),
                    Mode::Subtract,
                    painted,
                );
                if m > 0.0 && m < 1.0 {
                    partial_total += 1;
                }
                let offset = ((y * WIDTH + x) * 4) as usize;
                let pixel = rendered.pixel(x, y).expect("a pixel of the stage");
                for (channel, &code) in pixel.iter().enumerate().take(3) {
                    let input = srgb_to_linear(source.rgba[offset + channel]);
                    let effect = input * 2.0_f64.powf(MASKED_EV);
                    let blended = blend(input, effect, m);
                    assert_code(
                        code,
                        linear_to_srgb_code(blended),
                        blended,
                        &format!("round {round}, ({x}, {y}) channel {channel}"),
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(erased_total > 0, "no erase stroke was sampled at all");
    assert!(
        partial_total > 200,
        "only {partial_total} partially covered samples: the sweep is all endpoints"
    );
    assert_eq!(checked, 12 * (WIDTH * HEIGHT * 3) as usize);
    println!(
        "{checked} rendered channels over a brush subtracting from a radial, {partial_total} of \
         them partially covered, {erased_total} erase strokes, all within the colour-study \
         tolerance of the reference"
    );
}
