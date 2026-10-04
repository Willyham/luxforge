//! A buffer's blocks against packing them whole, as every tick did before the buffer kept its
//! blocks: the same words and the same chunks written after every tick, over a drag's ticks and
//! random ones, and nothing compared or copied of a block handed again at the same place. Then the
//! key a link reads its blocks by.
use super::super::{
    ClipMarks, Coverage, CoverageComponent, CoverageMode, GpuProgram, GpuSpatial, GpuStep, GpuTail,
    MaskedColour, PositionMap, chain::written_key, mask::changed_ranges,
};
use super::*;

/// The chunk the stage writes its blocks in.
const CHUNK: usize = super::super::BLOCK_CHUNK;

/// What a tick wrote when it packed the blocks whole: every step's blocks in each step's own
/// order — a masked step's components then its units, a spatial step's program then its mask's
/// components — at least one word, compared with the old packing chunk by chunk.
fn whole(old: &[u32], steps: &[GpuStep]) -> (Vec<u32>, Vec<Range<usize>>, bool) {
    let mut new = Vec::new();
    for step in steps {
        match step {
            GpuStep::Colour { program, .. } => new.extend_from_slice(&program.block),
            GpuStep::Masked(masked) => {
                for component in &masked.mask.components {
                    new.extend_from_slice(&component.program.block);
                }
                for unit in &masked.units {
                    new.extend_from_slice(&unit.block);
                }
            }
            GpuStep::Geometry(tail) => new.extend_from_slice(tail.block()),
            GpuStep::Spatial(spatial) => {
                new.extend_from_slice(&spatial.program.block);
                for component in spatial.mask.iter().flat_map(|mask| &mask.components) {
                    new.extend_from_slice(&component.program.block);
                }
            }
            GpuStep::Clipping(_) => {}
        }
    }
    if new.is_empty() {
        new.push(0);
    }
    let ranges = changed_ranges(old, &new, CHUNK);
    let changed = !ranges.is_empty() || old.len() != new.len();
    (new, ranges, changed)
}

/// Each range as its first word and the word after its last.
fn spans(ranges: &[Range<usize>]) -> Vec<(usize, usize)> {
    ranges
        .iter()
        .map(|range| (range.start, range.end))
        .collect()
}

fn block(words: impl IntoIterator<Item = u32>) -> Arc<[u32]> {
    words.into_iter().collect()
}

fn program(block: &Arc<[u32]>) -> GpuProgram {
    GpuProgram {
        block: Arc::clone(block),
        ..GpuProgram::new("test", "")
    }
}

fn mask(components: &[&Arc<[u32]>]) -> Coverage {
    Coverage {
        position: PositionMap::IDENTITY,
        bounds: [0, 0, 64, 64],
        supersample: false,
        components: components
            .iter()
            .map(|block| CoverageComponent {
                mode: CoverageMode::Add,
                invert: false,
                program: program(block),
            })
            .collect(),
        invert: false,
        scale: 1.0,
    }
}

fn colour(block: &Arc<[u32]>) -> GpuStep {
    GpuStep::colour(program(block))
}

fn masked(components: &[&Arc<[u32]>], units: &[&Arc<[u32]>]) -> GpuStep {
    GpuStep::Masked(MaskedColour {
        units: units.iter().map(|block| program(block)).collect(),
        position: PositionMap::IDENTITY,
        mask: mask(components),
    })
}

fn spatial(block: &Arc<[u32]>, components: &[&Arc<[u32]>]) -> GpuStep {
    GpuStep::Spatial(Box::new(GpuSpatial {
        program: program(block),
        planes: Vec::new(),
        passes: Vec::new(),
        applies: Vec::new(),
        clamps: false,
        mask: (!components.is_empty()).then(|| mask(components)),
        halos: Vec::new(),
    }))
}

/// A warp's tail, its nodes the block.
fn tail(nodes: &Arc<[u32]>) -> GpuStep {
    GpuStep::Geometry(GpuTail::grid(
        (64, 64),
        [0, 0, 64, 64],
        false,
        (0, 0),
        8,
        (9, 9),
        Arc::clone(nodes),
    ))
}

fn marks() -> GpuStep {
    GpuStep::Clipping(ClipMarks {
        shadows: true,
        highlights: true,
        shadow_below: 0.0,
        highlight_from: 1.0,
        palette: [[0; 4]; 3],
    })
}

/// One tick of `steps` into `written`, held to packing them whole over `old`, the words the buffer
/// held: the same words, chunks and change. Answers the update, and the words now held.
fn tick(written: &mut WrittenBlocks, old: &[u32], steps: &[GpuStep], what: &str) -> Update {
    let (new, ranges, changed) = whole(old, steps);
    let update = written.update(steps, CHUNK);
    assert_eq!(written.words(), new, "{what}: the words held");
    assert_eq!(update.ranges, ranges, "{what}: the chunks written");
    assert_eq!(update.changed, changed, "{what}: whether they changed");
    update
}

/// A drag's ticks, each held to packing every block whole: one that hands every block again at
/// the same place compares, copies and writes none of it; one whose block is a new allocation
/// compares that block alone and writes the chunks where its words differ, none when they do not;
/// a brush's block that grows moves every block after it, which are compared and copied as packing
/// them whole does; steps removed, added before and after the others, every block empty and the
/// buffer forgotten each write what packing the whole did, or every chunk once forgotten.
#[test]
fn a_buffers_blocks_follow_whole_packing_over_a_drag() {
    let curve = block(0..300);
    let brush = block((0..700).map(|word| word * 7));
    let unit = block([1, 2, 3, 4, 5]);
    let kernel = block(1000..1020);
    let cover = block(2000..2040);
    let grid = block((0..2000).map(|word| word ^ 0x5555));
    let mut written = WrittenBlocks::default();
    let mut old: Vec<u32> = Vec::new();
    let mut step = |written: &mut WrittenBlocks, steps: Vec<GpuStep>, what: &str| {
        let update = tick(written, &old, &steps, what);
        old = written.words().to_vec();
        update
    };
    let drag = |curve: &Arc<[u32]>, brush: &Arc<[u32]>, grid: &Arc<[u32]>| {
        vec![
            colour(curve),
            masked(&[brush], &[&unit]),
            spatial(&kernel, &[&cover]),
            tail(grid),
            marks(),
        ]
    };
    let first = step(&mut written, drag(&curve, &brush, &grid), "the first");
    assert_eq!(
        spans(&first.ranges),
        [(0, written.words().len())],
        "the first writes everything"
    );
    // The same blocks again, as a drag's every tick hands them: nothing compared or copied.
    for what in ["the same blocks", "the same blocks again"] {
        let update = step(&mut written, drag(&curve, &brush, &grid), what);
        assert_eq!((update.compared, update.copied), (0, 0), "{what}");
        assert!(update.ranges.is_empty() && !update.changed, "{what}");
    }
    // One word of the curve: its block alone compared, the chunk it lies in written.
    let mut words: Vec<u32> = curve.to_vec();
    words[10] = 77;
    let curve = block(words);
    let update = step(&mut written, drag(&curve, &brush, &grid), "a curve's word");
    assert_eq!((update.compared, update.copied), (300, 256));
    assert_eq!(spans(&update.ranges), [(0, 256)]);
    // The same words in a new allocation: compared, not copied, nothing written.
    let grid = block(grid.iter().copied());
    let update = step(
        &mut written,
        drag(&curve, &brush, &grid),
        "a new grid alike",
    );
    assert_eq!((update.compared, update.copied), (2000, 0));
    assert!(update.ranges.is_empty() && !update.changed);
    // A brush's stroke appends its segments and rewrites the index after them: every block after
    // it moves.
    let mut words: Vec<u32> = brush.to_vec();
    words.truncate(690);
    words.extend((0..47).map(|word| 9000 + word));
    let brush = block(words);
    step(
        &mut written,
        drag(&curve, &brush, &grid),
        "a brush that grew",
    );
    let update = step(
        &mut written,
        drag(&curve, &brush, &grid),
        "after the brush grew",
    );
    assert_eq!((update.compared, update.copied), (0, 0));
    // A step removed, one added after the others, one before them.
    let mut steps = drag(&curve, &brush, &grid);
    steps.remove(0);
    step(&mut written, steps.clone(), "the curve removed");
    let late = block(5000..5100);
    steps.push(colour(&late));
    let update = step(&mut written, steps.clone(), "a step added last");
    assert_eq!(update.compared + update.copied, 100, "only the new block");
    steps.insert(0, colour(&curve));
    step(&mut written, steps.clone(), "a step added first");
    // Every block empty: the buffer holds one zero word; then blocks again.
    let empty = block([]);
    step(
        &mut written,
        vec![colour(&empty), marks()],
        "no block words",
    );
    step(
        &mut written,
        vec![colour(&empty), marks()],
        "no block words again",
    );
    step(&mut written, steps.clone(), "blocks again");
    // A shrinking brush, as a stroke undone mid-drag.
    let brush = block(brush.iter().copied().take(400));
    step(
        &mut written,
        drag(&curve, &brush, &grid),
        "a brush that shrank",
    );
    // Forgotten — new buffers — the next tick writes every chunk, as a first write does, and
    // copies none of the blocks it already holds.
    written.forget();
    let steps = drag(&curve, &brush, &grid);
    let update = written.update(&steps, CHUNK);
    let (new, ranges, _) = whole(&[], &steps);
    assert_eq!(written.words(), new);
    assert_eq!(update.ranges, ranges, "every chunk");
    assert!(update.changed);
    assert_eq!((update.compared, update.copied), (0, 0));
    let update = written.update(&steps, CHUNK);
    assert!(update.ranges.is_empty() && !update.changed, "then nothing");
}

/// A small deterministic generator, so the random ticks are the same every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Over thousands of random ticks — blocks handed again, replaced by new ones alike or differing
/// in a few words, grown, shrunk, emptied, steps added and removed, the buffer forgotten — the
/// buffer's words and the chunks each tick writes are exactly packing every block whole's, and a
/// tick that hands every block again at its place compares and copies nothing.
#[test]
fn a_buffers_blocks_follow_whole_packing_over_random_ticks() {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let fresh = |random: &mut Random| -> Arc<[u32]> {
        let len = match random.below(4) {
            0 => 0,
            1 => random.below(8),
            2 => random.below(400),
            _ => random.below(1500),
        };
        (0..len).map(|_| random.below(3) as u32).collect()
    };
    // Each step a kind and its blocks: a colour step, a masked step of one or two components and a
    // unit, a spatial step with a mask or none, a tail.
    let mut steps: Vec<(u64, Vec<Arc<[u32]>>)> = Vec::new();
    let build = |steps: &[(u64, Vec<Arc<[u32]>>)]| -> Vec<GpuStep> {
        steps
            .iter()
            .map(|(kind, blocks)| match kind {
                0 => colour(&blocks[0]),
                1 => masked(&[&blocks[0], &blocks[1]], &[&blocks[2]]),
                2 => spatial(&blocks[0], &[&blocks[1]]),
                3 => spatial(&blocks[0], &[]),
                _ => tail(&blocks[0]),
            })
            .collect()
    };
    let counts = [1, 3, 2, 1, 1];
    let mut written = WrittenBlocks::default();
    let mut old: Vec<u32> = Vec::new();
    let (mut quiet, mut moved) = (0, 0);
    for number in 0..4000 {
        let mut held = true;
        match random.below(10) {
            // Every block handed again.
            0..=2 => {}
            // A block replaced: alike, a few words differing, grown or shrunk.
            3..=5 if !steps.is_empty() => {
                let at = random.below(steps.len() as u64) as usize;
                let blocks = &mut steps[at].1;
                let which = random.below(blocks.len() as u64) as usize;
                let mut words = blocks[which].to_vec();
                match random.below(4) {
                    0 => {}
                    1 => {
                        for _ in 0..=random.below(3) {
                            if !words.is_empty() {
                                let word = random.below(words.len() as u64) as usize;
                                words[word] = words[word].wrapping_add(1);
                            }
                        }
                    }
                    2 => words.extend((0..random.below(300)).map(|word| word as u32)),
                    _ => {
                        let keep = random.below(words.len() as u64 + 1) as usize;
                        words.truncate(keep);
                    }
                }
                blocks[which] = words.into();
                held = false;
            }
            // A step added or removed.
            6 | 7 if steps.len() < 8 => {
                let kind = random.below(5);
                let blocks = (0..counts[kind as usize])
                    .map(|_| fresh(&mut random))
                    .collect();
                let at = random.below(steps.len() as u64 + 1) as usize;
                steps.insert(at, (kind, blocks));
                held = false;
            }
            8 if !steps.is_empty() => {
                let at = random.below(steps.len() as u64) as usize;
                steps.remove(at);
                held = false;
            }
            _ => {}
        }
        // New buffers, with any of the changes above or none.
        if random.below(16) == 0 {
            written.forget();
            old.clear();
            held = false;
        }
        let built = build(&steps);
        let update = tick(&mut written, &old, &built, &format!("tick {number}"));
        if held && number > 0 {
            assert_eq!(
                (update.compared, update.copied),
                (0, 0),
                "tick {number}: every block handed again"
            );
            quiet += 1;
        }
        moved += usize::from(update.compared + update.copied > 0);
        old = written.words().to_vec();
    }
    assert!(
        quiet > 500 && moved > 500,
        "{quiet} quiet ticks, {moved} that moved"
    );
}

/// A link's key reads its blocks by their contents: the same blocks again, or alike in new
/// allocations, give the same key, a word changed another; a block packed again keeps its hash,
/// so a tick that hands every block again hashes none; empty blocks add nothing. The key a link's
/// output is named by follows its input, its words, its blocks' key and its pipeline.
#[test]
fn a_buffers_key_follows_its_blocks_contents() {
    let curve = block(0..300);
    let grid = block(0..2000);
    let empty = block([]);
    let steps =
        |curve: &Arc<[u32]>, grid: &Arc<[u32]>| vec![colour(curve), colour(&empty), tail(grid)];
    let mut written = WrittenBlocks::default();
    written.update(&steps(&curve, &grid), CHUNK);
    let key = written.key();
    written.update(&steps(&curve, &grid), CHUNK);
    assert_eq!(written.key, Some(key), "kept while the blocks are");
    assert!(
        written
            .sources
            .iter()
            .all(|source| source.block.is_empty() || source.hash.is_some()),
        "every block hashed once and its hash kept"
    );
    // Alike in a new allocation: hashed again, the same key.
    let alike = block(grid.iter().copied());
    written.update(&steps(&curve, &alike), CHUNK);
    assert_eq!(written.key, None);
    assert_eq!(written.key(), key);
    // One word: another key.
    let mut words = alike.to_vec();
    words[1999] += 1;
    written.update(&steps(&curve, &block(words)), CHUNK);
    assert_ne!(written.key(), key);
    // Without the empty block, the same contents.
    let mut bare = WrittenBlocks::default();
    bare.update(&[colour(&curve), tail(&grid)], CHUNK);
    assert_eq!(bare.key(), key);
    // The output's key.
    let named = written_key(1, &[1, 2], key, 4);
    assert_eq!(named, written_key(1, &[1, 2], key, 4));
    assert_ne!(named, written_key(2, &[1, 2], key, 4));
    assert_ne!(named, written_key(1, &[1, 3], key, 4));
    assert_ne!(named, written_key(1, &[1, 2], key + 1, 4));
    assert_ne!(named, written_key(1, &[1, 2], key, 5));
}
