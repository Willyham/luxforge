//! TASK-024: the colour-constrained brush, against the frozen study and end to end through the
//! service every client reaches.
//!
//! The mathematics is `docs/design/mask-study.md#the-colour-constraint` and the oracle is the
//! independent `f64` reference at `crates/luxforge-reference/src/mask.rs`, which shares no code with production.
//! What is asserted is **bit-identity** and not a tolerance: the similarity is the frozen colour
//! range's own falloff at one sample, written in the reference's order, so the two agree in their
//! last bits or the transcription is wrong.
//!
//! What else is proved here, each named by the thing it protects:
//!
//! * **The seed is the host's, never the client's.** `mask.add-stroke` carries a flag and a path and
//!   no colour at all, and the host reads the pixel the operation the mask modulates receives at the
//!   position the stroke began. A client cannot put a colour the photograph does not have there into
//!   a stroke, because there is no field for one.
//! * **The seed is stored, and read once.** It is part of the stroke's canonical bytes, so a limited
//!   stroke reproduces its own limit after any later edit and nothing is sampled again at render
//!   time.
//! * **The sample equals the render.** A point sample through a limited stroke equals the rendered
//!   byte, on the same shared call every other component reaches the mask through.
//! * **It is a colour test and not Auto Mask.** A limited stroke stops at a colour boundary it
//!   crosses *and* paints the same colour on the far side of the frame, which is the difference the
//!   user guide states in one sentence.

use super::*;
use luxforge_core::{
    AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EditorService, Layer, LayerId,
    Mask, ModuleRegistry, Mutation, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage,
    mask::{
        CompiledMask,
        commands::{self, MaskTarget},
    },
    path::{ColourLimit, Stroke, StrokeTable},
};
use luxforge_reference::mask::{
    Brush as RefBrush, BrushStroke, ColourLimit as RefColourLimit, Stage as RefStage,
    brush_coverage, colour_similarity,
};
use luxforge_reference::srgb_to_linear;
use luxforge_testkit::fixtures::{render, sample};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One stroke, captured the way the host captures it, optionally limited to a colour.
fn stroke_of(points: &[[f64; 2]], limit: Option<ColourLimit>) -> Stroke {
    let stroke = Stroke::capture(points, 0.18, 55.0, 100.0, false).expect("a legal stroke");
    match limit {
        None => stroke,
        Some(limit) => stroke.with_colour_limit(limit),
    }
}

/// A limit on the pixel at one position of the source, which is the seed the host would read there
/// for a stack whose only layer is the masked one.
fn limit_at(source: &SourceImage, x: u32, y: u32, refine: f64) -> ColourLimit {
    let offset = ((y * WIDTH + x) * 4) as usize;
    ColourLimit::sampled(
        [
            source.rgba[offset],
            source.rgba[offset + 1],
            source.rgba[offset + 2],
        ],
        refine,
    )
    .expect("a legal refine")
}

/// A limit on an arbitrary colour, for the sweeps that do not need a particular one.
fn limit_of(seed: [u8; 3], refine: f64) -> ColourLimit {
    ColourLimit::sampled(seed, refine).expect("a legal refine")
}

/// The reference's view of the **stored** stroke, which is what production evaluates: the positions
/// snapped to the path grid, the radius quantized there, and the stored limit carried through
/// unchanged. Feeding the reference anything else would compare two different strokes.
fn as_reference(stroke: &Stroke) -> BrushStroke {
    BrushStroke {
        points: stroke.points().collect(),
        size: stroke.size(),
        feather: stroke.feather(),
        flow: stroke.flow(),
        erase: stroke.erase(),
        colour: stroke.colour_limit().map(|limit| RefColourLimit {
            seed: limit.seed(),
            refine: limit.refine(),
        }),
    }
}

/// A mask holding one add brush component over these strokes, with the table they resolve through.
fn brush_mask(strokes: &[Stroke]) -> (Mask, StrokeTable) {
    let mut table = StrokeTable::new("the constrained brush tests");
    let ids: Vec<Value> = strokes
        .iter()
        .map(|stroke| json!(table.insert(stroke.clone()).as_str()))
        .collect();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("brush");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "brush",
        json!({"strokes": ids}),
    ));
    (mask, table)
}

/// A source with two flat colour fields side by side and a soft seam between them: a blue "sky" on
/// the left and a red "roof" on the right, so a stroke drawn across the seam has one colour to hold
/// and one to leave.
fn two_colour_source() -> SourceImage {
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let sky = x < WIDTH / 2;
            // A little variation inside each field, so neither is a single value and the hold has to
            // be a family of colours rather than an equality.
            let jitter = ((x + y) % 5) as u8;
            if sky {
                rgba.extend([90 + jitter, 130 + jitter, 200 + jitter, 255]);
            } else {
                rgba.extend([190 + jitter, 70 + jitter, 60 + jitter, 255]);
            }
        }
    }
    SourceImage {
        width: WIDTH,
        height: HEIGHT,
        rgba: rgba.into(),
        fingerprint: "sha256:constrained-brush-fixture".into(),
        orientation: 1,
    }
}

/// The linear-sRGB pixel of the source at one position, which for a stack whose only layer is the
/// masked one **is** the input that layer receives.
fn source_pixel(source: &SourceImage, x: u32, y: u32) -> [f64; 3] {
    let offset = ((y * WIDTH + x) * 4) as usize;
    [
        srgb_to_linear(source.rgba[offset]),
        srgb_to_linear(source.rgba[offset + 1]),
        srgb_to_linear(source.rgba[offset + 2]),
    ]
}

fn masked_stack(mask: Mask, strokes: StrokeTable) -> Recipe {
    Recipe {
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
        strokes,
        artifacts: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// Bit-identity with the frozen reference
// ---------------------------------------------------------------------------

/// **A limited stroke is bit-identical to the frozen reference**, at every refine and over pixels
/// inside and outside the gamut.
///
/// The similarity is the colour range's own falloff at one sample and the multiply is the last one a
/// stroke performs, so any rewrite — a precomputed reciprocal of the radius, a reassociated
/// `profile * (amount * k)`, a Horner-form `smooth` — shows here as a differing bit pattern rather
/// than as a tolerance that still passes.
#[test]
fn a_limited_stroke_is_bit_identical_to_the_frozen_reference() {
    let mut rng = SplitMix64(0x0024_1DEA);
    let stage = stage(48, 32);
    let reference_stage = RefStage::new(48, 32);
    let mut compared = 0usize;
    let mut partial = 0usize;
    for refine in [0.0, 30.0, 50.0, 85.0, 100.0] {
        for seed in [[90u8, 130, 200], [190, 70, 60], [128, 128, 128]] {
            let stroke = stroke_of(
                &[[0.2, 0.4], [0.5, 0.55], [0.8, 0.45]],
                Some(limit_of(seed, refine)),
            );
            let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
            let compiled = CompiledMask::new(&mask, stage, &table).expect("a compiled mask");
            let expected = RefBrush {
                strokes: vec![as_reference(&stroke)],
            };
            for y in (0..32).step_by(3) {
                for x in (0..48).step_by(3) {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    let rgb = [
                        rng.next_range(-0.1, 1.3),
                        rng.next_range(-0.1, 1.3),
                        rng.next_range(-0.1, 1.3),
                    ];
                    let want = brush_coverage(&expected, &reference_stage, u, v, rgb);
                    assert_eq!(
                        compiled.coverage(x, y, rgb).to_bits(),
                        want.to_bits(),
                        "refine {refine} seed {seed:?} at ({x}, {y}) on {rgb:?}"
                    );
                    if want > 0.0 && want < 1.0 {
                        partial += 1;
                    }
                    compared += 1;
                }
            }
        }
    }
    assert!(compared >= 2000, "compared only {compared} pixels");
    assert!(
        partial > 100,
        "only {partial} pixels landed on a falloff: the sweep is all endpoints"
    );
    println!("{compared} pixels bit-identical, {partial} of them on a falloff");
}

/// A limited stroke makes its component read pixels; an unlimited one does not. That is what the
/// coverage overlay reads to refuse a mask it cannot draw honestly, and it is the one thing the
/// colour constraint costs the rest of the mask.
#[test]
fn a_limited_stroke_makes_its_component_read_pixels() {
    let stage = stage(48, 32);
    let plain = stroke_of(&[[0.3, 0.5], [0.7, 0.5]], None);
    let (mask, table) = brush_mask(std::slice::from_ref(&plain));
    let compiled = CompiledMask::new(&mask, stage, &table).expect("a compiled mask");
    assert!(
        !compiled.reads_pixels(),
        "an unlimited brush reads no pixel and must not claim to"
    );

    let limited = stroke_of(
        &[[0.3, 0.5], [0.7, 0.5]],
        Some(limit_of([90, 130, 200], 50.0)),
    );
    let (mask, table) = brush_mask(&[limited]);
    let compiled = CompiledMask::new(&mask, stage, &table).expect("a compiled mask");
    assert!(compiled.reads_pixels());

    // One limited stroke among unlimited ones is enough, because coverage is composed over all of
    // them and the component's answer is about the component.
    let mixed = brush_mask(&[
        plain.clone(),
        stroke_of(
            &[[0.4, 0.2], [0.4, 0.8]],
            Some(limit_of([128, 128, 128], 20.0)),
        ),
    ]);
    let compiled = CompiledMask::new(&mixed.0, stage, &mixed.1).expect("a compiled mask");
    assert!(compiled.reads_pixels());
}

/// The limit only ever removes coverage, so the conservative rectangle and the smallest feature are
/// the unlimited stroke's — stated rather than assumed, because a rectangle that grew would be a
/// rectangle outside which coverage is not exactly zero.
#[test]
fn a_limit_leaves_the_rectangle_and_the_feature_width_alone() {
    let stage = stage(60, 40);
    let path = [[0.25, 0.4], [0.6, 0.6]];
    let plain = stroke_of(&path, None);
    let limited = stroke_of(&path, Some(limit_of([90, 130, 200], 100.0)));
    let (plain_mask, plain_table) = brush_mask(&[plain]);
    let (limited_mask, limited_table) = brush_mask(&[limited]);
    let unlimited = CompiledMask::new(&plain_mask, stage, &plain_table).expect("a compiled mask");
    let held = CompiledMask::new(&limited_mask, stage, &limited_table).expect("a compiled mask");
    assert_eq!(held.bounds(), unlimited.bounds());
    assert_eq!(held.min_feature_px(stage), unlimited.min_feature_px(stage));
    // And the promise the rectangle makes still holds: outside it, coverage is exactly zero for
    // every pixel value, not merely small.
    let bounds = held.bounds();
    for y in 0..40 {
        for x in 0..60 {
            let inside = x >= bounds.x0 && x < bounds.x1() && y >= bounds.y0 && y < bounds.y1();
            if !inside {
                for rgb in [[0.11, 0.2, 0.38], [0.9, 0.1, 0.1], [0.0, 0.0, 0.0]] {
                    assert_eq!(held.coverage(x, y, rgb), 0.0, "at ({x}, {y})");
                }
            }
        }
    }
}

/// The limit is part of the stroke's canonical bytes: a limited stroke has its own content address,
/// reproduces its limit from the stored bytes alone, and two captures of the same path with the same
/// limit are the same stored object.
#[test]
fn a_stored_limit_is_reproducible_from_the_strokes_own_bytes() {
    let path = [[0.3, 0.5], [0.7, 0.5]];
    let limit = limit_of([90, 130, 200], 62.5);
    let plain = stroke_of(&path, None);
    let limited = stroke_of(&path, Some(limit));
    assert_ne!(
        plain.id(),
        limited.id(),
        "a limit is part of what a stroke is, so it must change the address"
    );
    assert_eq!(
        limited.id(),
        stroke_of(&path, Some(limit)).id(),
        "the same path and the same limit is the same stored stroke"
    );
    // A different seed or a different refine is a different stroke.
    assert_ne!(
        limited.id(),
        stroke_of(&path, Some(limit_of([90, 130, 201], 62.5))).id(),
        "one code of seed is a different stroke"
    );
    assert_ne!(
        limited.id(),
        stroke_of(&path, Some(limit_of([90, 130, 200], 63.0))).id(),
        "a different refine is a different stroke"
    );

    let bytes = limited.canonical();
    let reparsed = Stroke::from_stored(&limited.id(), &bytes).expect("the stored bytes reparse");
    assert_eq!(reparsed.colour_limit(), Some(limit));
    assert_eq!(reparsed.id(), limited.id());
    // An unlimited stroke's bytes carry no colour field at all, so its spelling and its address are
    // what they have always been.
    let text = String::from_utf8(plain.canonical()).expect("canonical bytes are JSON");
    assert!(!text.contains("colour"), "{text}");

    // The refine is refused by name rather than clamped, and there is no seed to refuse: a code is
    // legal by construction, which is one reason the stored form is codes.
    for refine in [101.0, -1.0, f64::NAN] {
        let refused = ColourLimit::sampled([90, 130, 200], refine).unwrap_err();
        assert!(refused.to_string().contains("colour refine"), "{refused}");
    }
    // It is quantized to the tenth its declared control moves in, so a stored limit is exact.
    assert_eq!(limit_of([90, 130, 200], 62.54).refine(), 62.5);
    assert_eq!(limit_of([90, 130, 200], 62.55).refine(), 62.6);
}

// ---------------------------------------------------------------------------
// What it does to a picture, and what it does not
// ---------------------------------------------------------------------------

/// **A limited stroke holds the colour it was seeded on and leaves the one beside it** — and it does
/// so by colour and not by edge, so it also paints the seeded colour on the far side of a boundary
/// the stroke reaches. That is the difference from Lightroom's Auto Mask, measured rather than
/// asserted in prose, and it is the sentence the user guide gives a person to act on.
#[test]
fn a_limited_stroke_holds_its_colour_and_knows_nothing_about_edges() {
    let source = two_colour_source();
    let stage = stage(WIDTH, HEIGHT);
    // A wide stroke straight across the seam, seeded on the sky at its start.
    let limit = limit_at(&source, 2, HEIGHT / 2, 50.0);
    let seed = limit.seed();
    let across = [[0.1, 0.5], [0.9, 0.5]];
    let limited = stroke_of(&across, Some(limit));
    let plain = stroke_of(&across, None);
    let (limited_mask, limited_table) = brush_mask(&[limited]);
    let (plain_mask, plain_table) = brush_mask(&[plain]);
    let held = CompiledMask::new(&limited_mask, stage, &limited_table).expect("a compiled mask");
    let unlimited = CompiledMask::new(&plain_mask, stage, &plain_table).expect("a compiled mask");

    let row = HEIGHT / 2;
    let sky = source_pixel(&source, 2, row);
    let roof = source_pixel(&source, WIDTH - 3, row);
    // On the sky the limited stroke is the unlimited stroke, bit for bit.
    assert_eq!(
        held.coverage(2, row, sky).to_bits(),
        unlimited.coverage(2, row, sky).to_bits()
    );
    // On the roof it is exactly nothing, while the unlimited stroke covers it fully.
    assert_eq!(held.coverage(WIDTH - 3, row, roof), 0.0);
    assert!(unlimited.coverage(WIDTH - 3, row, roof) > 0.9);
    // And the sentence the guide has to be able to make: a matching colour *anywhere the stroke
    // reaches* is painted, edge or no edge. A patch of sky on the roof side, under the stroke, is
    // held exactly as the sky on the left is.
    assert_eq!(
        held.coverage(WIDTH - 3, row, sky).to_bits(),
        unlimited.coverage(WIDTH - 3, row, sky).to_bits(),
        "the limit is a colour test: it holds its colour wherever the stroke reaches it"
    );
    // The similarity itself is what does it, and the roof is outside the radius entirely.
    assert_eq!(
        colour_similarity(&RefColourLimit { seed, refine: 50.0 }, roof),
        0.0
    );
}

/// **A sampled byte equals the rendered byte through a limited stroke.**
///
/// `render.sample` and the rasterizing pass reach the mask through one call that now carries the
/// pixel, and a limited stroke is evaluated on the input the masked operation receives. A point query
/// handed the wrong pixel — the operation's output, or a neighbour — would disagree on the colour
/// boundary, which is where this sweep concentrates.
#[test]
fn a_sampled_byte_equals_the_rendered_byte_through_a_limited_stroke() {
    let source = two_colour_source();
    let registry = ModuleRegistry::builtin();
    // A tight refine, so the hold falls away inside the jitter of the sky field itself and much of
    // the frame sits on the similarity's own ramp rather than at one of its ends.
    let limited = stroke_of(
        &[[0.05, 0.5], [0.95, 0.5]],
        Some(limit_at(&source, 2, HEIGHT / 2, 92.0)),
    );
    let (mask, table) = brush_mask(std::slice::from_ref(&limited));
    let stack = masked_stack(mask, table);
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a frame");
    let reference_stage = RefStage::new(WIDTH, HEIGHT);
    let expected = RefBrush {
        strokes: vec![as_reference(&limited)],
    };
    let mut partial = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let sampled = sample(&registry, &source, &stack, x, y)
                .expect("a sample of the stage")
                .rgba
                .expect("inside the stage");
            let byte = rendered.pixel(x, y).expect("a pixel of the stage");
            assert_eq!(
                [sampled[0], sampled[1], sampled[2]],
                [byte[0], byte[1], byte[2]],
                "a sampled byte differs from the rendered byte at ({x}, {y})"
            );
            let (u, v) = reference_stage.pixel_uv(x, y);
            let m = brush_coverage(
                &expected,
                &reference_stage,
                u,
                v,
                source_pixel(&source, x, y),
            );
            if m > 0.0 && m < 1.0 {
                partial += 1;
            }
        }
    }
    assert!(
        partial > 20,
        "only {partial} pixels are on a falloff: the sweep proves too little"
    );
    println!(
        "{partial} pixels on the limit's own ramp, every sampled byte equal to the rendered one"
    );
}

// ---------------------------------------------------------------------------
// The host seeds the stroke, end to end
// ---------------------------------------------------------------------------

struct Fixture {
    service: EditorService,
    asset: AssetId,
    request: u64,
}

impl Fixture {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(luxforge_testkit::fixtures::jpeg(), &source).expect("the fixture copies");
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).expect("a catalog");
        let asset = service
            .import(&source)
            .expect("the fixture imports")
            .asset
            .id;
        Self {
            service,
            asset,
            request: 0,
        }
    }

    fn mutation(&mut self) -> Mutation {
        self.request += 1;
        Mutation {
            expected_revision: self.service.state(&self.asset).unwrap().revision,
            request_id: format!("constrained-{}", self.request),
            actor: "constrained-brush".to_owned(),
        }
    }

    fn mask_command(
        &mut self,
        method: &str,
        target: MaskTarget,
        parameters: Value,
    ) -> Result<luxforge_core::ActionResult, luxforge_core::Error> {
        let command = commands::find(method).expect("a declared command");
        let mutation = self.mutation();
        self.service.run_action(
            &self.asset,
            mutation,
            command.method,
            target.request(parameters),
        )
    }

    fn edit(&mut self, action: &str, parameters: Value) {
        let mutation = self.mutation();
        self.service
            .apply_action(&self.asset, mutation, action, parameters)
            .unwrap_or_else(|error| panic!("{action}: {error}"));
    }

    /// The current entry's recipe, read back through `entry` so its stroke references are resolved
    /// against the store — which is the path every reader takes and the reason a payload holds
    /// addresses rather than coordinates.
    fn recipe(&self) -> Recipe {
        let entry = self.service.state(&self.asset).unwrap().current_entry.id;
        self.service
            .entry(&self.asset, &entry)
            .expect("the current entry")
            .snapshot
            .recipe
            .clone()
    }
}

/// One brush stroke, unlimited, that draws a new mask.
fn painted(fixture: &mut Fixture) -> MaskTarget {
    let result = fixture
        .mask_command(
            commands::ADD_STROKE,
            MaskTarget::default(),
            json!({
                "points": [[0.3, 0.4], [0.6, 0.6]],
                "size": 0.18,
                "feather": 55.0,
                "flow": 100.0,
                "erase": false,
            }),
        )
        .expect("a first stroke draws a mask");
    MaskTarget {
        mask: result.mask,
        component: result.component,
        ..MaskTarget::default()
    }
}

/// **The host seeds a limited stroke from the pixel the masked operation receives**, and the request
/// carries no colour at all.
///
/// This is the whole of what makes the seed trustworthy: there is no parameter a client could put a
/// colour in, `mask.add-stroke` declares a flag and a refine, and the colour that ends up in the
/// stored stroke is the one `mask.sample-input` answers at the same position — the pixel the layer
/// bound to this mask receives, not the finished frame's.
#[test]
fn the_host_seeds_a_limited_stroke_from_the_masked_operations_input() {
    let mut fixture = Fixture::open("seeded");
    let target = painted(&mut fixture);
    let mask = target.mask.clone().expect("a mask was drawn");
    // Bind an adjustment to the mask, so there is an operation to be the input of.
    fixture.edit("set-basic", json!({"mask": mask.as_str(), "exposure": 1.5}));

    // A stroke whose start is a position the host will read the input pixel at.
    let start = [0.42, 0.37];
    let limited = fixture
        .mask_command(
            commands::ADD_STROKE,
            MaskTarget {
                mask: Some(mask.clone()),
                component: target.component.clone(),
                ..MaskTarget::default()
            },
            json!({
                "points": [start, [0.7, 0.5]],
                "size": 0.12,
                "feather": 40.0,
                "flow": 90.0,
                "erase": false,
                "limit_to_colour": true,
                "colour_refine": 64.0,
            }),
        )
        .expect("a limited stroke commits");
    assert_eq!(limited.label.as_deref(), Some("Update Brush 1"));

    // The stored stroke carries the limit, and the seed is the host's own answer at that position.
    let recipe = fixture.recipe();
    let component = recipe.masks[0].components[0].clone();
    let ids = luxforge_core::path::references(&component.payload, "the brush component")
        .expect("the reserved field");
    assert_eq!(ids.len(), 2, "two strokes on the one component");
    let stored = recipe
        .strokes
        .get(&ids[1])
        .expect("the second stroke")
        .clone();
    let held = stored.colour_limit().expect("the stroke carries its limit");
    assert_eq!(held.refine(), 64.0);

    let entry = fixture
        .service
        .state(&fixture.asset)
        .unwrap()
        .current_entry
        .id;
    // The same position the host seeded from, in the stage the masked layer receives.
    let answered = fixture
        .service
        .mask_input_sample(
            &fixture.asset,
            &entry,
            &mask,
            pixel_of(&fixture, &mask, start[0], true),
            pixel_of(&fixture, &mask, start[1], false),
        )
        .expect("the host answers its own sample");
    assert_eq!(
        [answered.r, answered.g, answered.b],
        held.seed(),
        "the stored seed is not the pixel the host reads at the stroke's start"
    );

    // The same request twice is the same stored stroke: the seed is read from the picture, so it is
    // deterministic, and the store addresses it by content.
    let again = fixture
        .mask_command(
            commands::ADD_STROKE,
            MaskTarget {
                mask: Some(mask.clone()),
                component: target.component.clone(),
                ..MaskTarget::default()
            },
            json!({
                "points": [start, [0.7, 0.5]],
                "size": 0.12,
                "feather": 40.0,
                "flow": 90.0,
                "erase": false,
                "limit_to_colour": true,
                "colour_refine": 64.0,
            }),
        )
        .expect("a second identical limited stroke commits");
    assert!(again.label.is_some(), "a second pass is a second stroke");
    let recipe = fixture.recipe();
    let ids = luxforge_core::path::references(
        &recipe.masks[0].components[0].payload,
        "the brush component",
    )
    .expect("the reserved field");
    assert_eq!(ids[1], ids[2], "the same request is the same stored stroke");

    // And there is no way to send a colour: the command declares none.
    let declared = commands::find(commands::ADD_STROKE).expect("the command");
    for name in ["seed", "seed_r", "r", "g", "b", "colour", "colour_seed"] {
        assert!(
            declared.action.parameter(name).is_none(),
            "mask.add-stroke must declare no colour parameter, and it declares {name}"
        );
    }
}

/// The content pixel index of a normalized coordinate in the stage the mask's layer receives. The
/// test asks the host for the stage rather than assuming the fixture's size.
fn pixel_of(fixture: &Fixture, mask: &luxforge_core::MaskId, value: f64, horizontal: bool) -> u32 {
    let entry = fixture
        .service
        .state(&fixture.asset)
        .unwrap()
        .current_entry
        .id;
    // `mask.sample-input` reports the stage it read from, so one probe at the origin answers it.
    let probe = fixture
        .service
        .mask_input_sample(&fixture.asset, &entry, mask, 0, 0)
        .expect("a probe at the origin");
    let side = if horizontal {
        probe.width
    } else {
        probe.height
    };
    (value * f64::from(side)).floor() as u32
}

/// A limit on a mask no layer is bound to is **refused by name**, and so is one on the stroke that
/// draws a new mask.
///
/// Refusing is the honest answer: the limit reads the pixel the operation a mask modulates receives,
/// and a mask with no operation has no such pixel. Seeding from the source or from the finished frame
/// instead would put the seed in a different domain from the one the similarity is evaluated in — the
/// range study measures a `+0.75 EV` layer taking a sky from fully selected to not selected at all —
/// so the stroke would quietly paint nothing.
#[test]
fn a_limit_without_an_operation_to_read_is_refused_by_name() {
    let mut fixture = Fixture::open("refused");
    // The stroke that draws a mask cannot be limited: there is no mask yet, so no layer either.
    let error = fixture
        .mask_command(
            commands::ADD_STROKE,
            MaskTarget::default(),
            json!({
                "points": [[0.3, 0.4], [0.6, 0.6]],
                "size": 0.18,
                "feather": 55.0,
                "flow": 100.0,
                "erase": false,
                "limit_to_colour": true,
            }),
        )
        .expect_err("a new mask has no operation to read an input from");
    assert!(error.to_string().contains("draws a new mask"), "{error}");

    // A mask that exists but carries no layer is refused too, and the refusal names it.
    let target = painted(&mut fixture);
    let mask = target.mask.clone().expect("a mask was drawn");
    let error = fixture
        .mask_command(
            commands::ADD_STROKE,
            MaskTarget {
                mask: Some(mask.clone()),
                component: target.component.clone(),
                ..MaskTarget::default()
            },
            json!({
                "points": [[0.4, 0.4], [0.6, 0.6]],
                "size": 0.12,
                "feather": 40.0,
                "flow": 90.0,
                "erase": false,
                "limit_to_colour": true,
            }),
        )
        .expect_err("a mask with no layer has no operation");
    let message = error.to_string();
    assert!(message.contains("Mask 1"), "{message}");
    assert!(message.contains("no layer is bound"), "{message}");

    // `mask.sample-input` refuses the same case, by the same rule and in the same words.
    let entry = fixture
        .service
        .state(&fixture.asset)
        .unwrap()
        .current_entry
        .id;
    let error = fixture
        .service
        .mask_input_sample(&fixture.asset, &entry, &mask, 4, 4)
        .expect_err("no layer is bound to the mask");
    assert!(error.to_string().contains("no layer is bound"), "{error}");

    // Bind a layer and both work, which is what makes the refusal a state and not a wall.
    fixture.edit("set-basic", json!({"mask": mask.as_str(), "exposure": 1.0}));
    let entry = fixture
        .service
        .state(&fixture.asset)
        .unwrap()
        .current_entry
        .id;
    let answered = fixture
        .service
        .mask_input_sample(&fixture.asset, &entry, &mask, 4, 4)
        .expect("the host answers once there is an operation");
    assert!(answered.r.is_finite() && answered.g.is_finite() && answered.b.is_finite());
    assert!(answered.width > 0 && answered.height > 0);
}

/// The host's own canvas pick is declared for every sampling kind, targets host methods, and submits
/// exactly the fields that kind's sample command declares.
///
/// This is proposal P17's mechanism, asserted as a property of the tables rather than of a list of
/// names: registering a sampling kind is what gives it a pick, and the delivered "every top-level
/// number field of the result whose name is a parameter of the action" rule needs no exception
/// because `mask.sample-input` answers `r`, `g` and `b` and `mask.add-<kind>-sample` declares them.
#[test]
fn the_hosts_canvas_pick_runs_a_host_query_into_a_host_command() {
    let kinds: Vec<&str> = luxforge_core::mask::sampling_kinds().collect();
    assert!(!kinds.is_empty(), "some kind samples colours");
    assert_eq!(commands::canvas().len(), kinds.len());
    for (kind, pick) in kinds.iter().zip(commands::canvas()) {
        let luxforge_core::CanvasInteraction::SampleApply {
            query,
            x,
            y,
            action,
            title,
            shortcut,
        } = pick
        else {
            panic!("a mask's pick is a sample-apply");
        };
        assert_eq!(query, commands::SAMPLE_INPUT);
        assert_eq!(action, &format!("mask.add-{kind}-sample"));
        assert!(shortcut.is_none(), "a mask's pick is offered by its panel");
        assert!(title.starts_with("Pick"), "{title}");
        // The query declares the two coordinates the pick fills.
        // The query is one of the host descriptor's reads, never one of its actions.
        assert!(
            commands::find(query).is_none(),
            "a pick's query writes nothing"
        );
        let sample_input = commands::find_query(query).expect("the query is a declared host read");
        for name in [x, y] {
            let declared = sample_input
                .parameter(name)
                .unwrap_or_else(|| panic!("{query} declares {name}"));
            assert!(matches!(
                declared.kind,
                luxforge_core::ParameterKind::Integer { .. }
            ));
        }
        // And the action declares exactly the fields the query answers with.
        let add = commands::find(action).expect("the action is a declared host command");
        for name in ["r", "g", "b"] {
            assert!(
                add.action.parameter(name).is_some(),
                "{action} declares {name}"
            );
        }
        // The pick's mode is its action's method, so a client needs no second table.
        assert!(commands::canvas_pick(action).is_some());
        assert!(commands::canvas_pick("mask.list").is_none());
    }
}
