//! The blocks a slot and each link of its chain write, through the photograph's own draw on a
//! headless device: a tick that hands a block again at the same place — a warp's grid, a curve's,
//! a brush's — neither compares nor copies it; one that changes a block writes the chunks packing
//! every block whole and comparing it with the last tick's would; and every frame is the one a
//! fresh slot, written whole, draws.
use super::super::{blocks::block_len, chain, mask::changed_ranges};
use super::*;

/// Adds its block's value for the stage column, repeating every word-0 columns, to every channel.
fn offsets(values: &[f32]) -> GpuProgram {
    GpuProgram {
        words: vec![values.len() as u32],
        block: values.iter().map(|value| value.to_bits()).collect(),
        ..GpuProgram::new(
            "offsets",
            "fn offsets(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             return rgb + vec3<f32>(lf_block_f32(block + u32(pos.x) % lf_word(words)));\n}\n",
        )
    }
}

/// Coverage read from its block, one word a column, repeating every word-0 columns.
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

/// A coordinate grid of a node at every pixel edge, `SIDE + 1` a side, each at its own place but
/// those `shifted` names, moved a quarter pixel right: the identity warp but there. Its nodes are
/// the tail's block, 8450 words.
fn grid(shifted: &[(u32, u32)]) -> Arc<[u32]> {
    let side = SIDE + 1;
    (0..side * side)
        .flat_map(|node| {
            let (column, row) = (node % side, node / side);
            let moved = if shifted.contains(&(column, row)) {
                0.25
            } else {
                0.0
            };
            [column as f32 + moved, row as f32].map(f32::to_bits)
        })
        .collect()
}

fn tail(nodes: &Arc<[u32]>) -> GpuStep {
    GpuStep::Geometry(GpuTail::grid(
        (SIDE, SIDE),
        [0, 0, SIDE, SIDE],
        false,
        (0, 0),
        1,
        (SIDE + 1, SIDE + 1),
        Arc::clone(nodes),
    ))
}

/// What each counter of the blocks' writes has reached: words compared, copied and written.
fn counts(pipeline: &PhotoPipeline) -> [u64; 3] {
    let figures = &pipeline.figures.preview;
    [
        figures.block_compared.load(Ordering::Acquire),
        figures.block_copied.load(Ordering::Acquire),
        figures.block_words.load(Ordering::Acquire),
    ]
}

/// Each link's blocks, packed whole as a tick packed them before the slot kept its blocks: every
/// link before the last, then the last.
fn packed(plan: &GpuPlan) -> Vec<Vec<u32>> {
    let split = chain::chain(&plan.steps);
    split
        .links
        .iter()
        .copied()
        .chain(std::iter::once(split.last))
        .map(|steps| {
            let (mut words, mut blocks) = (Vec::new(), Vec::new());
            chain::pack_steps(plan.texels, (0, 0), steps, &mut words, &mut blocks);
            assert_eq!(blocks.len(), block_len(steps));
            blocks
        })
        .collect()
}

/// How many words packing every link's blocks whole, and comparing each with the last tick's chunk
/// by chunk, writes from `old`'s packing to `new`'s.
fn written_whole(old: &GpuPlan, new: &GpuPlan) -> u64 {
    packed(old)
        .iter()
        .zip(packed(new))
        .map(|(old, new)| {
            changed_ranges(old, &new, BLOCK_CHUNK)
                .iter()
                .map(|range| range.len() as u64)
                .sum::<u64>()
        })
        .sum()
}

/// What the slot's and each link's buffer holds, the last's last.
fn held(pipeline: &PhotoPipeline) -> Vec<Vec<u32>> {
    let slot = pipeline.surfaces[&ID].gpu.as_ref().expect("a slot");
    slot.chain
        .iter()
        .map(|link| link.written_blocks().words().to_vec())
        .chain(std::iter::once(slot.written_blocks.words().to_vec()))
        .collect()
}

/// The frame a fresh slot draws for `plan`, every block written whole: `reference`'s slot released
/// first.
fn fresh(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    reference: &mut PhotoPipeline,
    plan: &GpuPlan,
) -> Vec<u8> {
    paint(device, queue, reference, &primitive(ID, None));
    settle(reference);
    let drawn = paint(device, queue, reference, &primitive(ID, Some(plan.clone())));
    assert_eq!(
        diagnostics(reference, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
    drawn
}

/// One tick of `plan`: drawn on the GPU, every buffer holding its link's blocks packed whole and
/// the frame a fresh slot's. Answers the words it compared, copied and wrote, and how many passes
/// it submitted.
fn tick(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (pipeline, reference): (&mut PhotoPipeline, &mut PhotoPipeline),
    plan: &GpuPlan,
    what: &str,
) -> ([u64; 3], u64) {
    let (before, passes) = (
        counts(pipeline),
        diagnostics(pipeline, ID).gpu_preview_passes,
    );
    let drawn = paint(device, queue, pipeline, &primitive(ID, Some(plan.clone())));
    assert_eq!(
        diagnostics(pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu),
        "{what}"
    );
    assert_eq!(
        held(pipeline),
        packed(plan),
        "{what}: what the buffers hold"
    );
    let expected = fresh(device, queue, reference, plan);
    let differing = drawn
        .chunks_exact(4)
        .zip(expected.chunks_exact(4))
        .filter(|(drawn, expected)| drawn != expected)
        .count();
    assert_eq!(differing, 0, "{what}: pixels unlike a fresh slot's");
    let now = counts(pipeline);
    (
        std::array::from_fn(|index| now[index] - before[index]),
        diagnostics(pipeline, ID).gpu_preview_passes - passes,
    )
}

/// A drag over a lens warp's grid, its 8450 words the tail's block: each tick that hands the grid
/// again, the colour step's word changing, draws its frame and compares, copies and writes none of
/// the grid. A grid alike in a new allocation is compared and nothing written; one with a node
/// moved writes the chunk it lies in, as comparing the whole packing would.
#[test]
fn a_drag_over_a_warp_grid_compares_and_copies_none_of_it() {
    let test = "a_drag_over_a_warp_grid_compares_and_copies_none_of_it";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let mut reference = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_with_codes(1);
    let nodes = grid(&[]);
    let warp = |factor: f32, nodes: &Arc<[u32]>| GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::colour(scale(factor)), tail(nodes)],
        region: None,
    };
    let both = (&mut pipeline, &mut reference);
    let first = warp(0.5, &nodes);
    let (written, _) = tick(&device, &queue, (both.0, both.1), &first, "the first");
    assert_eq!(
        written[2],
        block_len(&first.steps) as u64,
        "the first writes it whole"
    );
    let mut last = first;
    for factor in [0.6, 0.7, 0.8] {
        let plan = warp(factor, &nodes);
        let (written, passes) = tick(&device, &queue, (both.0, both.1), &plan, "a drag");
        assert_eq!(
            written,
            [0, 0, 0],
            "{factor}: none of the grid compared or copied"
        );
        assert_eq!(passes, 1, "{factor}: the tick's frame drawn");
        last = plan;
    }
    // Alike, in a new allocation: compared, nothing copied or written, nothing drawn again.
    let alike: Arc<[u32]> = nodes.iter().copied().collect();
    let plan = warp(0.8, &alike);
    let (written, passes) = tick(&device, &queue, (both.0, both.1), &plan, "alike");
    assert_eq!(written, [nodes.len() as u64, 0, 0]);
    assert_eq!(passes, 0, "an unchanged plan encodes nothing");
    // A node moved: the chunk it lies in.
    let moved = grid(&[(40, 40)]);
    let plan = warp(0.8, &moved);
    let expected = written_whole(&last, &plan);
    let (written, passes) = tick(&device, &queue, (both.0, both.1), &plan, "a node moved");
    assert_eq!(expected, BLOCK_CHUNK as u64, "one chunk");
    assert_eq!(written, [nodes.len() as u64, BLOCK_CHUNK as u64, expected]);
    assert_eq!(passes, 1);
}

/// A chain of three links — a colour link reading a block, a spatial link with a block of its own
/// and a masked step whose coverage a brush-like block holds, then the last link's colour step and
/// a warp's grid — writes through each link's own buffer only the blocks a tick changes: a drag of
/// the last link's word compares and copies no link's blocks and runs no link before the last,
/// its first tick writing the spatial link's blocks whole once, as the planes the first tick made
/// left it to; a first link's block changed, a brush's block grown so a block after it moves, a
/// grid alike in a new allocation and a step removed each write what packing every link whole
/// would. Every frame is a fresh slot's.
#[test]
fn a_chains_links_write_only_the_blocks_a_tick_changes() {
    let test = "a_chains_links_write_only_the_blocks_a_tick_changes";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let mut reference = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_with_codes(1);
    let ramp = |count: usize, scale: f32| -> Vec<f32> {
        (0..count)
            .map(|index| (index % 17) as f32 * scale)
            .collect()
    };
    struct Blocks {
        first: GpuProgram,
        kernel: Arc<[u32]>,
        brush: GpuProgram,
        after: Option<GpuProgram>,
        nodes: Arc<[u32]>,
        factor: f32,
    }
    let plan_of = |blocks: &Blocks| {
        let mut spatial = super::spatial::test_spatial();
        spatial.program.block = Arc::clone(&blocks.kernel);
        let mut steps = vec![
            GpuStep::colour(blocks.first.clone()),
            GpuStep::Spatial(Box::new(spatial)),
            GpuStep::Masked(MaskedColour {
                units: vec![scale(1.25)],
                position: PositionMap::IDENTITY,
                mask: Coverage {
                    position: PositionMap::IDENTITY,
                    bounds: [0, 0, SIDE, SIDE],
                    supersample: false,
                    components: vec![CoverageComponent {
                        mode: CoverageMode::Add,
                        invert: false,
                        program: blocks.brush.clone(),
                    }],
                    invert: false,
                    scale: 1.0,
                },
            }),
        ];
        steps.extend(blocks.after.clone().map(GpuStep::colour));
        steps.extend([
            GpuStep::Spatial(Box::new(super::spatial::test_spatial())),
            GpuStep::colour(scale(blocks.factor)),
            tail(&blocks.nodes),
        ]);
        GpuPlan {
            boundary: boundary.clone(),
            texels: TexelMap::IDENTITY,
            steps,
            region: None,
        }
    };
    let mut blocks = Blocks {
        first: offsets(&ramp(300, 0.002)),
        kernel: (0..16).collect(),
        brush: columns(&ramp(700, 1.0 / 16.0)),
        after: Some(offsets(&ramp(64, 0.001))),
        nodes: grid(&[]),
        factor: 0.5,
    };
    let link_passes = |pipeline: &PhotoPipeline| {
        let slot = pipeline.surfaces[&ID].gpu.as_ref().expect("a slot");
        slot.chain[1]
            .spatial
            .as_ref()
            .expect("the spatial link's planes")
            .dispatched
    };
    let first = plan_of(&blocks);
    assert_eq!(chain::chain(&first.steps).links.len(), 2, "three links");
    let (written, _) = tick(
        &device,
        &queue,
        (&mut pipeline, &mut reference),
        &first,
        "the first",
    );
    assert_eq!(
        written[2],
        packed(&first)
            .iter()
            .map(|link| link.len() as u64)
            .sum::<u64>(),
        "the first writes every link whole"
    );
    let mut last = first;
    // A drag of the last link's word: no link's blocks compared or copied, no link before the
    // last run. The planes the first tick made for the spatial link, after it wrote its blocks,
    // forgot what its buffers hold, so the drag's first tick writes that link's blocks whole once,
    // copying none of them.
    for (tick_number, factor) in [0.6, 0.7, 0.8].into_iter().enumerate() {
        blocks.factor = factor;
        let plan = plan_of(&blocks);
        let ran = link_passes(&pipeline);
        let (written, passes) = tick(
            &device,
            &queue,
            (&mut pipeline, &mut reference),
            &plan,
            "a drag",
        );
        let rewritten = if tick_number == 0 {
            packed(&plan)[1].len() as u64
        } else {
            0
        };
        assert_eq!(written, [0, 0, rewritten], "{factor}");
        assert_eq!(passes, 1, "{factor}");
        assert_eq!(
            link_passes(&pipeline),
            ran,
            "{factor}: the spatial link not run"
        );
        last = plan;
    }
    // Each change, with the words it compares, and every write held to packing the whole.
    type Change = fn(&mut Blocks, &dyn Fn(usize, f32) -> Vec<f32>);
    let changes: [(&str, Change, Option<u64>); 5] = [
        (
            "a value of the first link's block",
            |blocks, ramp| {
                let mut values = ramp(300, 0.002);
                values[100] = 0.25;
                blocks.first = offsets(&values);
            },
            Some(300),
        ),
        (
            "the brush grown, the block after it moved",
            |blocks, ramp| blocks.brush = columns(&ramp(740, 1.0 / 16.0)),
            None,
        ),
        (
            "the grid alike in a new allocation",
            |blocks, _| blocks.nodes = blocks.nodes.iter().copied().collect(),
            Some(8450),
        ),
        (
            "the step after the brush removed",
            |blocks, _| blocks.after = None,
            Some(0),
        ),
        ("nothing", |_, _| {}, Some(0)),
    ];
    for (what, change, compared) in changes {
        change(&mut blocks, &ramp);
        let plan = plan_of(&blocks);
        let expected = written_whole(&last, &plan);
        let (written, _) = tick(
            &device,
            &queue,
            (&mut pipeline, &mut reference),
            &plan,
            what,
        );
        assert_eq!(written[2], expected, "{what}: as packing every link whole");
        if let Some(compared) = compared {
            assert_eq!(written[0], compared, "{what}: the words compared");
        }
        last = plan;
    }
}
