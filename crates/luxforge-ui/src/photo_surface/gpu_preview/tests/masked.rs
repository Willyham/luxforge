//! The masked colour step on its own: its assembly, words and signature without a device, then its
//! coverage, algebra, supersample, bounds and blend read back from a headless device as `f32`, and a
//! tick's block writes through the photograph's real draw. The coverage programs here are written
//! for the tests; the core's own, one per mask kind, are qualified against their CPU fields by the
//! desktop's tests (`cargo test -p luxforge-app gpu_mask`).
use super::*;
use crate::photo_surface::gpu_preview::{
    mask::changed_ranges,
    qualification::{self, Qualifier},
};

// ---- Test programs ----------------------------------------------------------------------------

/// A coverage program: `(pos + ½) / width` along x (word 0 is 0) or y (word 0 is 1), clamped.
fn ramp(axis: u32, width: f32) -> GpuProgram {
    GpuProgram {
        words: vec![axis, width.to_bits()],
        ..GpuProgram::new(
            "ramp",
            "fn ramp(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
             let axis = select(pos.x, pos.y, lf_word(words) == 1u);\n    \
             return clamp((axis + 0.5) / lf_f32(words + 1u), 0.0, 1.0);\n}\n",
        )
    }
}

/// A hard edge: one left of `pos.x = edge`, zero from it on.
fn edge(at: f32) -> GpuProgram {
    GpuProgram {
        words: vec![at.to_bits()],
        ..GpuProgram::new(
            "edge",
            "fn edge(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
             return select(0.0, 1.0, pos.x < lf_f32(words));\n}\n",
        )
    }
}

/// A value-based component: the green of the operation's input.
fn green() -> GpuProgram {
    GpuProgram::new(
        "green",
        "fn green(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
         return rgb.g;\n}\n",
    )
}

/// Coverage read from its block, one word a column, repeating every `values.len()` columns.
fn columns(values: &[f32]) -> GpuProgram {
    GpuProgram {
        words: vec![values.len() as u32],
        block: values.iter().map(|value| value.to_bits()).collect(),
        ..GpuProgram::new(
            "columns",
            "fn columns(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
             return lf_block_f32(block + u32(pos.x) % lf_word(words));\n}\n",
        )
    }
}

/// Every channel one: over a black boundary, a masked step of it outputs its coverage.
fn white() -> GpuProgram {
    GpuProgram::new(
        "white",
        "fn white(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return vec3<f32>(1.0);\n}\n",
    )
}

/// Every channel over its word, zero here: infinite or not a number wherever it runs.
fn poison() -> GpuProgram {
    GpuProgram {
        words: vec![0f32.to_bits()],
        ..GpuProgram::new(
            "poison",
            "fn poison(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             return (rgb + vec3<f32>(1.0)) / lf_f32(words);\n}\n",
        )
    }
}

fn component(mode: CoverageMode, invert: bool, program: GpuProgram) -> CoverageComponent {
    CoverageComponent {
        mode,
        invert,
        program,
    }
}

/// A mask over the whole `side` × `side` stage at the stage's own pixels.
fn coverage(side: u32, components: Vec<CoverageComponent>) -> Coverage {
    Coverage {
        position: PositionMap::IDENTITY,
        bounds: [0, 0, side, side],
        supersample: false,
        components,
        invert: false,
        scale: 1.0,
    }
}

fn masked(units: Vec<GpuProgram>, mask: Coverage) -> GpuStep {
    GpuStep::Masked(MaskedColour {
        units,
        position: PositionMap::IDENTITY,
        mask,
    })
}

/// The CPU's fold of one pixel's component values, in `f64`.
fn fold(mask: &Coverage, values: &[f64]) -> f64 {
    let mut m = 0.0f64;
    for (component, value) in mask.components.iter().zip(values) {
        let c = if component.invert {
            1.0 - value
        } else {
            *value
        };
        m = match component.mode {
            CoverageMode::Add => m.max(c),
            CoverageMode::Subtract => m.min(1.0 - c),
            CoverageMode::Intersect => m.min(c),
        };
    }
    let m = if mask.invert { 1.0 - m } else { m };
    f64::from(mask.scale) * m
}

fn black(side: u32) -> GpuBoundary {
    qualification::boundary(side, side, 1, &vec![[0.0; 3]; (side * side) as usize]).unwrap()
}

fn plan_of(boundary: GpuBoundary, steps: Vec<GpuStep>) -> GpuPlan {
    GpuPlan {
        boundary,
        texels: TexelMap::IDENTITY,
        steps,
        region: None,
        lights: Vec::new(),
    }
}

// ---- Without a device -------------------------------------------------------------------------

#[test]
fn a_masked_step_assembles_its_coverage_once_and_validates() {
    let first = masked(
        vec![scale(0.5), swap(true)],
        coverage(
            64,
            vec![
                component(CoverageMode::Add, false, ramp(0, 64.0)),
                component(CoverageMode::Subtract, true, green()),
                component(CoverageMode::Intersect, false, columns(&[1.0, 0.5])),
            ],
        ),
    );
    let second = masked(
        vec![scale(2.0)],
        coverage(64, vec![component(CoverageMode::Add, false, ramp(1, 32.0))]),
    );
    let steps = vec![
        GpuStep::colour(scale(0.25)),
        first,
        second,
        GpuStep::colour(identity()),
    ];
    for step in &steps {
        validate_step(step).expect("each program against its role's signature");
    }
    let source = assemble(&steps).expect("an assembled shader");
    validate(&source).unwrap_or_else(|error| panic!("{error}\n{source}"));
    assert_eq!(source.matches("fn lf_surface_compose(").count(), 1);
    assert_eq!(
        source.matches("fn ramp(").count(),
        1,
        "one source, two calls"
    );
    assert_eq!(source.matches("fn scale(").count(), 1);
    assert!(source.contains("fn lf_surface_mask_1("));
    assert!(source.contains("fn lf_surface_mask_2("));
    assert!(source.contains("lf_surface_mask_1(stage, lf_surface_input)"));
    // Without a masked step, the shader is as it was.
    let plain = assemble(&[GpuStep::colour(identity())]).unwrap();
    assert!(!plain.contains("lf_surface"));

    // A component with a colour signature, or a unit with a coverage one, is refused by name.
    let wrong_component = masked(
        vec![identity()],
        coverage(64, vec![component(CoverageMode::Add, false, identity())]),
    );
    let error = validate_step(&wrong_component).unwrap_err();
    assert!(
        error.contains("(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32"),
        "{error}"
    );
    let wrong_unit = masked(
        vec![green()],
        coverage(64, vec![component(CoverageMode::Add, false, green())]),
    );
    let error = validate_step(&wrong_unit).unwrap_err();
    assert!(error.contains("-> vec3<f32>"), "{error}");
    // The names the surface generates are its own.
    let reserved = masked(
        vec![identity()],
        coverage(
            64,
            vec![component(
                CoverageMode::Add,
                false,
                GpuProgram::new(
                    "lf_surface_mine",
                    "fn lf_surface_mine(pos: vec2<f32>, rgb: vec3<f32>, words: u32, \
                     block: u32) -> f32 { return 1.0; }\n",
                ),
            )],
        ),
    );
    assert!(
        validate_step(&reserved)
            .unwrap_err()
            .contains("surface's own")
    );
    assert!(assemble(&[reserved]).unwrap_err().contains("surface's own"));
}

/// A masked step's words: its header slot holds its bases and the units' position map; from its
/// base, the mask's map, bounds, flags and scale, each component's mode, inversion and bases, each
/// unit's bases, then the programs' own words; its block is the components' blocks then the units'.
#[test]
fn a_masked_steps_words_are_its_mask_then_its_programs() {
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        1,
        1,
        1,
        [[0.0; 4]],
    )
    .unwrap();
    let mask = Coverage {
        position: PositionMap {
            a: 0,
            b: -1,
            tx: 7,
            c: 1,
            d: 0,
            ty: -2,
        },
        bounds: [1, 2, 30, 40],
        supersample: true,
        components: vec![
            component(CoverageMode::Subtract, true, ramp(1, 8.0)),
            component(CoverageMode::Intersect, false, columns(&[0.25, 0.75])),
        ],
        invert: true,
        scale: 0.6,
    };
    let step = GpuStep::Masked(MaskedColour {
        units: vec![scale(3.0), stripe(0.5, 2)],
        position: PositionMap {
            tx: 5,
            ..PositionMap::IDENTITY
        },
        mask,
    });
    let chain = GpuPlan {
        boundary,
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::colour(scale(0.5)), step],
        region: None,
        lights: Vec::new(),
    };
    let (mut words, mut blocks) = (Vec::new(), Vec::new());
    pack(&chain, &mut words, &mut blocks);
    let header = MAP_WORDS + STEP_WORDS * 2;
    let f = |value: f32| value.to_bits();
    // The masked step's header slot: its bases, after the first step's one word, and its units'
    // position map.
    let slot = &words[MAP_WORDS + STEP_WORDS..MAP_WORDS + 2 * STEP_WORDS];
    let base = header + 1;
    assert_eq!(slot[..2], [base as u32, 0]);
    assert_eq!(slot[2..], [1.0, 0.0, 5.0, 0.0, 1.0, 0.0].map(f));
    let own = &words[base..];
    assert_eq!(own[..6], [0.0, -1.0, 7.0, 1.0, 0.0, -2.0].map(f));
    assert_eq!(own[6..10], [1.0, 2.0, 30.0, 40.0].map(f));
    assert_eq!(own[10..13], [1, 1, f(0.6)]);
    // Components: mode, inversion, then their words' and blocks' bases.
    let programs = base + 13 + 4 * 2 + 2 * 2;
    assert_eq!(own[13..17], [1, 1, programs as u32, 0]);
    assert_eq!(own[17..21], [2, 0, programs as u32 + 2, 0]);
    // Units: the ramp's two words and the columns' one before them, the columns' two block words.
    assert_eq!(own[21..23], [programs as u32 + 3, 2]);
    assert_eq!(own[23..25], [programs as u32 + 4, 2]);
    assert_eq!(words[programs..], [1, f(8.0), 2, f(3.0)]);
    assert_eq!(blocks, [f(0.25), f(0.75), f(0.5), 2]);
}

#[test]
fn a_tick_writes_only_the_chunks_of_the_blocks_that_changed() {
    let spans = |old: &[u32], new: &[u32]| -> Vec<(usize, usize)> {
        changed_ranges(old, new, 256)
            .into_iter()
            .map(|range| (range.start, range.end))
            .collect()
    };
    let old: Vec<u32> = (0..1000).collect();
    assert!(spans(&old, &old).is_empty(), "nothing changed");
    // Appended words: the chunk they start in and the ones after.
    let mut grown = old.clone();
    grown.extend(1000..1300);
    assert_eq!(spans(&old, &grown), [(768, 1300)]);
    // One word in the middle: its chunk alone; two chunks apart, two ranges; neighbours merge.
    let mut touched = old.clone();
    touched[300] = 7;
    assert_eq!(spans(&old, &touched), [(256, 512)]);
    touched[900] = 7;
    assert_eq!(spans(&old, &touched), [(256, 512), (768, 1000)]);
    touched[600] = 7;
    assert_eq!(spans(&old, &touched), [(256, 1000)]);
    // A shorter block writes nothing past its own end, and a first write is the whole block.
    assert!(spans(&old, &old[..900]).is_empty());
    assert_eq!(spans(&[], &old), [(0, 1000)]);
}

// ---- On a headless device ---------------------------------------------------------------------

/// The coverage the masked step composes, read back as `f32` through a unit that outputs one over a
/// black boundary, against the CPU's fold of the same component values in `f64`: every mode, every
/// inversion, the whole mask's inversion and the amount.
#[test]
fn the_algebra_inversions_and_amount_compose_as_the_cpus() {
    let test = "the_algebra_inversions_and_amount_compose_as_the_cpus";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    eprintln!("{test}: {}", qualifier.adapter());
    let side = 64u32;
    let modes = [
        CoverageMode::Add,
        CoverageMode::Subtract,
        CoverageMode::Intersect,
    ];
    let mut cases = 0;
    for second in modes {
        for third in modes {
            for (inverts, invert, scale) in [
                ([false, false, false], false, 1.0),
                ([true, false, true], false, 0.75),
                ([false, true, false], true, 0.5),
                ([true, true, true], true, 0.3),
            ] {
                let mask = Coverage {
                    invert,
                    scale,
                    ..coverage(
                        side,
                        vec![
                            component(CoverageMode::Add, inverts[0], ramp(0, 64.0)),
                            component(second, inverts[1], ramp(1, 48.0)),
                            component(third, inverts[2], edge(40.0)),
                        ],
                    )
                };
                let plan = plan_of(black(side), vec![masked(vec![white()], mask.clone())]);
                let drawn = qualifier.evaluate(&plan).expect("a qualification pass");
                for (index, texel) in drawn.iter().enumerate() {
                    let (x, y) = (
                        f64::from(index as u32 % side),
                        f64::from(index as u32 / side),
                    );
                    let values = [
                        ((x + 0.5) / 64.0).clamp(0.0, 1.0),
                        ((y + 0.5) / 48.0).clamp(0.0, 1.0),
                        if x < 40.0 { 1.0 } else { 0.0 },
                    ];
                    let expected = fold(&mask, &values);
                    assert!(
                        (f64::from(texel[0]) - expected).abs() <= 1e-6,
                        "{second:?} {third:?} {inverts:?} {invert} {scale}: ({x}, {y}) drew {} \
                         for {expected}",
                        texel[0]
                    );
                    assert_eq!(texel[0], texel[1]);
                }
                cases += 1;
            }
        }
    }
    eprintln!(
        "{test}: {cases} compositions of {} pixels equal",
        side * side
    );
}

/// Under the thin-feature rule each component reads the four pixels `2q + (i, j)` of the doubled
/// stage, and coverage is their mean: a hard edge at a doubled-stage column draws a half-covered
/// column, as the CPU's supersample does.
#[test]
fn the_supersample_is_the_mean_over_the_doubled_stage() {
    let test = "the_supersample_is_the_mean_over_the_doubled_stage";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let side = 32u32;
    for (edge_at, supersample) in [(21.0, true), (21.0, false), (20.0, true)] {
        let mask = Coverage {
            supersample,
            scale: 0.8,
            ..coverage(
                side,
                vec![
                    component(CoverageMode::Add, false, edge(edge_at)),
                    component(CoverageMode::Intersect, false, ramp(1, 64.0)),
                ],
            )
        };
        let plan = plan_of(black(side), vec![masked(vec![white()], mask.clone())]);
        let drawn = qualifier.evaluate(&plan).expect("a qualification pass");
        for (index, texel) in drawn.iter().enumerate() {
            let (x, y) = (index as u32 % side, index as u32 / side);
            let at = |px: u32, py: u32| {
                fold(
                    &mask,
                    &[
                        if f64::from(px) < f64::from(edge_at) {
                            1.0
                        } else {
                            0.0
                        },
                        ((f64::from(py) + 0.5) / 64.0).clamp(0.0, 1.0),
                    ],
                )
            };
            let expected = if supersample {
                (at(2 * x, 2 * y)
                    + at(2 * x + 1, 2 * y)
                    + at(2 * x, 2 * y + 1)
                    + at(2 * x + 1, 2 * y + 1))
                    * 0.25
            } else {
                at(x, y)
            };
            assert!(
                (f64::from(texel[0]) - expected).abs() <= 1e-6,
                "edge {edge_at}, supersampled {supersample}: ({x}, {y}) drew {} for {expected}",
                texel[0]
            );
        }
    }
}

/// The mask's position map places it on its own stage and its bounds cut it: outside them, and
/// wherever coverage is exactly zero, the output is the input itself and no unit runs, so a unit
/// that is infinite everywhere reaches no pixel the mask does not cover.
#[test]
fn outside_the_bounds_and_where_coverage_is_zero_the_input_is_untouched() {
    let test = "outside_the_bounds_and_where_coverage_is_zero_the_input_is_untouched";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let side = 48u32;
    let pixels: Vec<[f32; 3]> = (0..side * side)
        .map(|index| {
            let (x, y) = (index % side, index / side);
            [x as f32 / 64.0, y as f32 / 64.0, 0.5]
        })
        .collect();
    let boundary = qualification::boundary(side, side, 1, &pixels).unwrap();
    // The mask's stage is the boundary's turned a quarter right: (x, y) -> (side - 1 - y, x).
    let turn = PositionMap {
        a: 0,
        b: -1,
        tx: side as i32 - 1,
        c: 1,
        d: 0,
        ty: 0,
    };
    let mask = Coverage {
        position: turn,
        bounds: [10, 4, 30, 44],
        ..coverage(side, vec![component(CoverageMode::Add, false, edge(20.0))])
    };
    let plan = plan_of(boundary, vec![masked(vec![poison()], mask)]);
    let drawn = qualifier.evaluate(&plan).expect("a qualification pass");
    let (mut untouched, mut poisoned) = (0, 0);
    for (index, texel) in drawn.iter().enumerate() {
        let (x, y) = (index as u32 % side, index as u32 / side);
        let (mx, my) = (side - 1 - y, x);
        let inside = (10..30).contains(&mx) && (4..44).contains(&my);
        let input = pixels[index].map(qualification::held);
        if inside && mx < 20 {
            assert_ne!(texel[..3], input, "({x}, {y}) is covered and runs the unit");
            poisoned += 1;
        } else {
            assert_eq!(texel[..3], input, "({x}, {y}) is the input itself");
            untouched += 1;
        }
    }
    assert!(poisoned > 0 && untouched > 0);
}

/// The blend is against the masked operation's own input, after the steps before it and before the
/// steps after it, with its units chained unclamped; a value-based component reads that same input.
#[test]
fn the_blend_and_a_value_based_component_read_the_operations_own_input() {
    let test = "the_blend_and_a_value_based_component_read_the_operations_own_input";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let side = 32u32;
    let pixels: Vec<[f32; 3]> = (0..side * side)
        .map(|index| {
            let (x, y) = (index % side, index / side);
            [0.25, x as f32 / 32.0, y as f32 / 16.0]
        })
        .collect();
    let boundary = qualification::boundary(side, side, 1, &pixels).unwrap();
    let mask = Coverage {
        scale: 0.5,
        ..coverage(side, vec![component(CoverageMode::Add, false, green())])
    };
    let plan = plan_of(
        boundary,
        vec![
            GpuStep::colour(scale(0.5)),
            masked(vec![scale(4.0), swap(true)], mask),
            GpuStep::colour(scale(2.0)),
        ],
    );
    let drawn = qualifier.evaluate(&plan).expect("a qualification pass");
    for (index, texel) in drawn.iter().enumerate() {
        let input = pixels[index].map(|value| f64::from(qualification::held(value)) * 0.5);
        // Coverage is the green of the masked operation's input, the halved boundary.
        let m = 0.5 * input[1];
        let units = [input[2] * 4.0, input[1] * 4.0, input[0] * 4.0];
        for channel in 0..3 {
            let expected = if m == 0.0 {
                input[channel]
            } else {
                (1.0 - m) * input[channel] + m * units[channel]
            } * 2.0;
            assert!(
                (f64::from(texel[channel]) - expected).abs() <= 2e-6 * expected.abs().max(1.0),
                "texel {index} channel {channel}: {} for {expected}",
                texel[channel]
            );
        }
    }
}

/// Through the photograph's own draw: a block that grows by appended words writes the chunks they
/// fall in, an unchanged one writes and encodes nothing, and one changed in the middle writes that
/// chunk; each frame draws what its block now says.
#[test]
fn a_growing_block_writes_its_new_chunks_and_draws_them() {
    let test = "a_growing_block_writes_its_new_chunks_and_draws_them";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    // Coverage per column from the block, one or zero; the block is padded past the 64 columns so
    // the columns sit at its end, where a brush's appended segments do.
    let frame = |values: &[f32]| {
        let mut block = vec![0.0f32; 700];
        block.extend_from_slice(values);
        let mut program = columns(&block);
        program.source = Cow::Borrowed(
            "fn columns(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
             return lf_block_f32(block + 700u + u32(pos.x) % lf_word(words));\n}\n",
        );
        program.words = vec![values.len() as u32];
        let plan = plan_of(
            black(SIDE),
            vec![masked(
                vec![white()],
                coverage(SIDE, vec![component(CoverageMode::Add, false, program)]),
            )],
        );
        primitive(ID, Some(plan))
    };
    let expect = |drawn: &[u8], values: &[f32]| {
        for (index, pixel) in drawn.chunks_exact(4).enumerate() {
            let column = index % SIDE as usize;
            let code = if values[column % values.len()] == 1.0 {
                255
            } else {
                0
            };
            assert_eq!(pixel[..3], [code; 3], "pixel {index}");
        }
    };
    let written =
        |pipeline: &PhotoPipeline| pipeline.figures.preview.block_words.load(Ordering::Acquire);
    let mut values: Vec<f32> = (0..40)
        .map(|column| (column % 3 == 0) as u8 as f32)
        .collect();
    let first = frame(&values);
    expect(&paint(&device, &queue, &mut pipeline, &first), &values);
    let total = 700 + values.len() as u64;
    assert_eq!(
        written(&pipeline),
        total,
        "the first frame writes the whole block"
    );
    // Appended columns: the chunk they start in to the end.
    values.extend((40..64).map(|column| (column % 2 == 0) as u8 as f32));
    let passes = pipeline.figures.preview.passes();
    expect(
        &paint(&device, &queue, &mut pipeline, &frame(&values)),
        &values,
    );
    let appended = written(&pipeline) - total;
    assert_eq!(appended, 764 - 512, "the third 256-word chunk and its tail");
    assert_eq!(pipeline.figures.preview.passes(), passes + 1);
    // Unchanged: nothing written, nothing encoded.
    let before = written(&pipeline);
    paint(&device, &queue, &mut pipeline, &frame(&values));
    assert_eq!(written(&pipeline), before);
    assert_eq!(pipeline.figures.preview.passes(), passes + 1);
    // One column changed: its chunk alone.
    values[10] = 1.0 - values[10];
    expect(
        &paint(&device, &queue, &mut pipeline, &frame(&values)),
        &values,
    );
    assert_eq!(written(&pipeline) - before, 764 - 512);
    assert_eq!(
        diagnostics(&pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
}
