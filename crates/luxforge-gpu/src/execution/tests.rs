use super::*;
/// The identity program: a pointwise colour program that returns its input.
pub(super) fn identity() -> GpuProgram {
    GpuProgram::new(
        "identity",
        "fn identity(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return rgb;\n}\n",
    )
}

/// Scales by its one uniform word.
pub(super) fn scale(factor: f32) -> GpuProgram {
    GpuProgram {
        words: vec![factor.to_bits()],
        ..GpuProgram::new(
            "scale",
            "fn scale(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             return rgb * lf_f32(words);\n}\n",
        )
    }
}

/// Swaps red and blue when its one word is 1.
fn swap(on: bool) -> GpuProgram {
    GpuProgram {
        words: vec![u32::from(on)],
        ..GpuProgram::new(
            "swap",
            "fn swap(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             if lf_word(words) == 1u {\n        return rgb.bgr;\n    }\n    return rgb;\n}\n",
        )
    }
}

/// Paints the grey of its block's first word on every stage pixel whose `x + y` is a multiple of
/// its block's second word.
fn stripe(grey: f32, period: u32) -> GpuProgram {
    GpuProgram {
        block: Arc::from([grey.to_bits(), period]),
        ..GpuProgram::new(
            "stripe",
            "fn stripe(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             let at = u32(pos.x) + u32(pos.y);\n    \
             if at % lf_block_word(block + 1u) == 0u {\n        \
             return vec3<f32>(lf_block_f32(block));\n    }\n    return rgb;\n}\n",
        )
    }
}

pub(super) fn plan(boundary: &GpuBoundary, programs: Vec<GpuProgram>) -> GpuPlan {
    GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: programs.into_iter().map(GpuStep::colour).collect(),
        region: None,
        lights: Vec::new(),
    }
}

// ---- Without a device -------------------------------------------------------------------------

/// What a caller's tests check of every program it hands over: the program alone, against the
/// prelude and the convention. The stage checks each program the same way before it compiles.
#[test]
fn a_step_is_checked_against_the_convention_on_its_own() {
    // The core names its entries lf_<module>_<unit>, as the Basic exposure program does, with its
    // helpers and constants after the entry.
    let core_named = GpuProgram::new(
        "lf_basic_exposure",
        "const lf_basic_exposure_floor: f32 = 0.0;\n\
         fn lf_basic_exposure_gain(words: u32) -> f32 { return lf_f32(words); }\n\
         fn lf_basic_exposure(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> {\n    return rgb * lf_basic_exposure_gain(words) + \
         lf_basic_exposure_floor;\n}\n",
    );
    for program in [
        identity(),
        scale(0.5),
        swap(true),
        stripe(0.5, 4),
        core_named,
    ] {
        validate_step(&GpuStep::colour(program.clone()))
            .unwrap_or_else(|error| panic!("{}: {error}", program.entry));
    }
    let colour = |entry: &'static str, source: &'static str| {
        validate_step(&GpuStep::colour(GpuProgram::new(entry, source))).unwrap_err()
    };
    let signature = "fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32)";
    for (what, error, expected) in [
        (
            "a binding of its own",
            colour(
                "bad",
                "@group(0) @binding(3) var<storage, read> bad_more: array<u32>;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * f32(bad_more[0]);\n}\n",
            ),
            "binding, global variable",
        ),
        (
            "a private global",
            colour(
                "bad",
                "var<private> bad_state: f32;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb;\n}\n",
            ),
            "binding, global variable",
        ),
        (
            "an entry point",
            colour(
                "bad",
                "fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb;\n}\n\
                 @compute @workgroup_size(1) fn bad_main() {}\n",
            ),
            "entry point",
        ),
        (
            "a coverage signature for a colour step",
            colour(
                "bad",
                "fn bad(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n\
                 return 1.0;\n}\n",
            ),
            signature,
        ),
        (
            "no entry function",
            colour("bad", "fn bad_helper(x: f32) -> f32 { return x; }\n"),
            "names no function",
        ),
        (
            "a parse error",
            colour(
                "bad",
                "fn bad(rgb: vec3<f32>) -> vec3<f32> { return rgb }\n",
            ),
            "expected",
        ),
        (
            "a helper not named after its entry",
            colour(
                "bad",
                "fn helper(x: f32) -> f32 { return x; }\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * helper(1.0);\n}\n",
            ),
            "does not start with its entry's name",
        ),
        (
            "a constant not named after its entry",
            colour(
                "bad",
                "const half_gain: f32 = 0.5;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * half_gain;\n}\n",
            ),
            "does not start with its entry's name",
        ),
        (
            "a named type",
            colour(
                "bad",
                "struct bad_pair { a: f32, b: f32 }\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 let p = bad_pair(1.0, 2.0);\n    return rgb * p.a;\n}\n",
            ),
            "declares the type",
        ),
        (
            "an override",
            colour(
                "bad",
                "override bad_gain: f32 = 1.0;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * bad_gain;\n}\n",
            ),
            "override",
        ),
        (
            "one of the surface's names",
            colour("lf_fragment", ""),
            "surface's own",
        ),
    ] {
        assert!(error.contains(expected), "{what}: {error}");
    }
}

#[test]
fn assembly_includes_a_shared_program_once_and_refuses_what_cannot_be_chained() {
    // Two layers of one unit: one source, two calls, each with its own base indices and its own
    // position map.
    let steps: Vec<GpuStep> = [scale(0.5), swap(true), scale(2.0)]
        .into_iter()
        .map(GpuStep::colour)
        .collect();
    let source = assemble(&steps).expect("an assembled shader");
    assert_eq!(source.matches("fn scale(").count(), 1);
    let call = |entry: &str, base: usize| {
        let word = |k: usize| format!("lf_f32({}u)", base + 2 + k);
        format!(
            "rgb = {entry}(rgb, vec2<f32>({} * stage.x + {} * stage.y + {}, \
             {} * stage.x + {} * stage.y + {}), lf_words[{base}u], lf_words[{}u]);",
            word(0),
            word(1),
            word(2),
            word(3),
            word(4),
            word(5),
            base + 1
        )
    };
    for (entry, base) in [("scale", 6), ("swap", 14), ("scale", 22)] {
        assert!(
            source.contains(&call(entry, base)),
            "{entry} at {base}:\n{source}"
        );
    }
    validate(&source).expect("the chain validates");
    validate(&assemble(&[]).expect("no steps")).expect("an empty chain is the identity");

    let refused = |program: GpuProgram| assemble(&[GpuStep::colour(program)]);
    for (entry, why) in [
        ("lf_boundary", "surface's own"),
        ("lf_word", "surface's own"),
        ("", "needs a name"),
        ("9lives", "identifier"),
        ("__hidden", "identifier"),
        ("two words", "identifier"),
    ] {
        let error = refused(GpuProgram::new(entry.to_owned(), "")).unwrap_err();
        assert!(error.contains(why), "{entry:?}: {error}");
    }
    let mut other = scale(0.5);
    other.source = Cow::Borrowed("fn scale(rgb: vec3<f32>) -> vec3<f32> { return rgb; }\n");
    let error = assemble(&[GpuStep::colour(scale(0.5)), GpuStep::colour(other)]).unwrap_err();
    assert!(error.contains("different sources"), "{error}");
}

#[test]
fn the_words_are_the_map_then_each_steps_bases_and_position_then_their_words() {
    let boundary =
        GpuBoundary::from_linear(crate::execution::BoundaryFormat::Half, 1, 1, 1, [[0.0; 4]])
            .unwrap();
    let mut chain = plan(&boundary, vec![scale(0.5), stripe(0.25, 3), swap(true)]);
    chain.texels = TexelMap {
        origin: [2.0, 5.0],
        step: [1.0, 0.5],
    };
    // The stripe's pos is the stage turned a quarter, 40 rows down.
    let turned = PositionMap {
        a: 0,
        b: -1,
        tx: 40,
        c: 1,
        d: 0,
        ty: 0,
    };
    chain.steps[1] = GpuStep::Colour {
        program: stripe(0.25, 3),
        position: turned,
    };
    let (mut words, mut blocks) = (Vec::new(), Vec::new());
    pack(&chain, &mut words, &mut blocks);
    let header = (MAP_WORDS + STEP_WORDS * 3) as u32;
    let unmoved = [1f32, 0.0, 0.0, 0.0, 1.0, 0.0].map(f32::to_bits);
    let mut expected = vec![
        2f32.to_bits(),
        5f32.to_bits(),
        1f32.to_bits(),
        0.5f32.to_bits(),
        // A whole frame's output starts at the stage's first pixel.
        0,
        0,
    ];
    // scale: its word after the header, no block.
    expected.extend([header, 0]);
    expected.extend(unmoved);
    // stripe: no words, the first two block words, its own map.
    expected.extend([header + 1, 0]);
    expected.extend([0f32, -1.0, 40.0, 1.0, 0.0, 0.0].map(f32::to_bits));
    // swap: one word, after the stripe's block.
    expected.extend([header + 1, 2]);
    expected.extend(unmoved);
    expected.extend([0.5f32.to_bits(), 1]);
    assert_eq!(words, expected);
    assert_eq!(blocks, [0.25f32.to_bits(), 3]);
    // Every block empty still binds one word.
    pack(&plan(&boundary, vec![identity()]), &mut words, &mut blocks);
    assert_eq!(blocks, [0]);
    // A region's output starts at its rectangle: in the boundary's texels with no tail, in the
    // stage with one.
    let mut region = plan(&boundary, vec![identity()]);
    region.texels.origin = [3.0, 4.0];
    region.region = Some(GpuRegion {
        rect: [10, 20, 30, 40],
        stage: (64, 64),
        full_stage: (64, 64),
    });
    pack(&region, &mut words, &mut blocks);
    assert_eq!(words[4..MAP_WORDS], [7, 16]);
    region.steps.insert(
        0,
        GpuStep::Geometry(GpuTail::affine(
            (20, 20),
            [0, 0, 1, 1],
            false,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        )),
    );
    pack(&region, &mut words, &mut blocks);
    assert_eq!(words[4..MAP_WORDS], [10, 20]);
}

#[test]
fn a_boundary_holds_exactly_its_half_float_texels() {
    let boundary = GpuBoundary::from_linear(
        crate::execution::BoundaryFormat::Half,
        2,
        1,
        9,
        [[0.5, -2.0, 1.0e5, 1.0], [0.0, 1.0, 0.1, 1.0]],
    )
    .expect("two texels");
    assert_eq!((boundary.size(), boundary.version()), ((2, 1), 9));
    let halves: Vec<f32> = (**boundary.texels.as_ref().expect("its texels"))
        .as_ref()
        .chunks_exact(2)
        .map(|bytes| half::f16::from_bits(u16::from_le_bytes([bytes[0], bytes[1]])).to_f32())
        .collect();
    assert_eq!(
        halves,
        [
            0.5,
            -2.0,
            f32::INFINITY,
            1.0,
            0.0,
            1.0,
            half::f16::from_f32(0.1).to_f32(),
            1.0
        ]
    );
    assert!(
        GpuBoundary::from_linear(crate::execution::BoundaryFormat::Half, 2, 1, 1, [[0.0; 4]])
            .is_none(),
        "too few"
    );
    assert!(
        GpuBoundary::from_linear(
            crate::execution::BoundaryFormat::Half,
            1,
            1,
            1,
            [[0.0; 4]; 2]
        )
        .is_none(),
        "too many"
    );
    let half = crate::execution::BoundaryFormat::Half;
    let float = crate::execution::BoundaryFormat::Float;
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 15]), 2, 1, 1, half).is_none());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 16]), 2, 1, 1, half).is_some());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 16]), 2, 1, 1, float).is_none());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 32]), 2, 1, 1, float).is_some());
    assert!(GpuBoundary::new(Arc::new(Vec::<u8>::new()), 0, 0, 1, half).is_none());
}

/// An `rgba32float` boundary holds every value as the `f32` it is, a near-black one's sign and a
/// value past the half range included, sixteen bytes a texel.
#[test]
fn a_float_boundary_holds_its_values_exactly() {
    let values = [[-3.0e-8, 1.0e5, 0.1, 1.0], [2.5e-9, -0.0, 7.0, 1.0]];
    let boundary =
        GpuBoundary::from_linear(crate::execution::BoundaryFormat::Float, 2, 1, 4, values)
            .expect("two texels");
    assert_eq!(boundary.bytes(), 32);
    let held: Vec<u32> = (**boundary.texels.as_ref().expect("its texels"))
        .as_ref()
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    let expected: Vec<u32> = values
        .iter()
        .flatten()
        .map(|value| value.to_bits())
        .collect();
    assert_eq!(held, expected);
}

#[test]
fn the_budget_refuses_an_allocation_past_it_and_names_itself() {
    let figures = Figures::default();
    figures.budget.store(100, Ordering::Release);
    figures.charge(60).expect("within the budget");
    assert_eq!(
        figures.charge(41),
        Err(GpuFallback::BudgetExceeded {
            requested: 41,
            in_use: 60,
            budget: 100
        })
    );
    assert_eq!(figures.in_use(), 60, "a refusal charges nothing");
    figures.charge(40).expect("exactly the budget");
    figures.discharge(100);
    assert_eq!((figures.in_use(), figures.peak()), (0, 100));
    assert_eq!(
        GpuFallback::BudgetExceeded {
            requested: 0,
            in_use: 0,
            budget: 0
        }
        .as_str(),
        "budget-exceeded"
    );
}

#[test]
fn a_device_without_fragment_storage_or_an_srgb_target_has_no_stage() {
    let srgb = wgpu::TextureFormat::Bgra8UnormSrgb;
    assert!(supported(&wgpu::Limits::default(), srgb));
    assert!(!supported(&wgpu::Limits::downlevel_webgl2_defaults(), srgb));
    assert!(!supported(
        &wgpu::Limits::default(),
        wgpu::TextureFormat::Bgra8Unorm
    ));
}

pub(super) fn headless(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    let (device, queue, _) = super::headless::device(test, wgpu::Limits::default())?;
    Some((device, queue))
}
pub(super) const SIDE: u32 = 64;
pub(super) fn held(code: u8) -> f32 {
    half::f16::from_f32(luxforge_reference::srgb::decode(code) as f32).to_f32()
}

pub(super) fn own_pipeline(device: &wgpu::Device, queue: &wgpu::Queue) -> crate::Executor {
    crate::Executor::with_figures(
        device,
        queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        Arc::default(),
    )
}
pub(super) fn settle(pipeline: &crate::Executor) {
    luxforge_testbase::wait_until("the released slots’ retirement", || {
        pipeline.figures.retirement_pending.load(Ordering::Acquire) == 0
    });
}
