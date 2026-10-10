//! A repair layer on the GPU (`docs/design/corrections.md`, "Rendering (TASK-003)"): the repair
//! boundary as one spatial step of the plan, its program the numerics study's `repair/repair.wgsl`
//! and its groups `repair/gpu.rs` describes (`docs/design/corrections-numerics.md`, "GPU form").
//!
//! - **Groups.** The operations evaluate in table order, grouped into ordered passes: a run of
//!   consecutive operations none of whose reads (destination with Heal's ring, source) meets an
//!   earlier destination of the run reads one input, so one set of dispatches over a per-level
//!   atlas serves every operation of it, independent dust spots costing one.
//! - **Chained.** One group alone is the step's one apply over its input. Groups before the last
//!   each end in a blend pass that writes the state the next reads — the window's own planes,
//!   alternating — and the last group's apply blends over the last state. Every group's block is
//!   the step's storage block from its own offset, which its passes' fourth word names.
//! - **Reads.** The step holds its read set itself (`RepairEntry::step_reads`): every rectangle its
//!   operations read, packed into atlas planes of its own ([`GpuReadAtlas`]). The photo surface
//!   cuts the step's input into the first from the photograph's source, or copies it from the
//!   boundary where the boundary holds it, before any pass reads it; the states chained groups
//!   leave take the same placements, and the read table at the start of the step's block takes a
//!   stage pixel to its atlas texel and back. So the step evaluates every operation from the whole
//!   frame's values whatever window its output is drawn over: a source across a 60 MP frame grows
//!   no window, and the window walk reads the repair's output window alone. A read set that does
//!   not pack into one atlas of [`REPAIR_ATLAS_SIDE`] texels a side is drawn by the reference,
//!   named.
//!
//! Planned on the catalog owner from the bound table: `O(operations + segments × nearby tiles +
//! tiles)` for the groups' blocks, no pixel read.
use super::{
    GpuFallback, GpuPass, GpuPassShape, GpuPlane, GpuPlaneFormat, GpuPlaneSize, GpuSpatial,
    plan::Planning, spatial::GpuApply,
};
use crate::{
    modules::{Region, RepairKind, Stage},
    render::repair_entry::RepairEntry,
};
use std::sync::Arc;

/// The most passes one repair step runs. Each group of conflicting operations runs its own set of
/// dispatches, a Heal's about `30 L` for `L` levels; past this many the reference draws the layer,
/// named, rather than a frame waiting behind thousands of dispatches.
pub const REPAIR_PASSES_MAX: usize = 2048;

/// The most bytes a repair step's Heal level planes may take on either GPU device. A layer whose
/// largest Heal needs more is drawn by the reference, which refuses past its own solve bound; the
/// GPU never solves a smaller problem in its place.
pub const REPAIR_LEVEL_BYTES: u64 = 512 * 1024 * 1024;

/// The repair boundary `entry` of layer `layer`, received at `stage`, as a step of a plan walked
/// with `planning`, or the named reason the reference draws it.
pub(crate) fn admit(
    layer: usize,
    entry: &RepairEntry,
    stage: Stage,
    planning: Planning,
) -> Result<GpuSpatial, GpuFallback> {
    if stage != entry.stage {
        return Err(GpuFallback::Repair {
            layer,
            why: format!(
                "it was bound to {}x{} and receives {}x{}",
                entry.stage.width, entry.stage.height, stage.width, stage.height
            ),
        });
    }
    if !crate::repair::REPAIR_PROGRAM.enabled && !planning.qualifying {
        return Err(GpuFallback::DisabledProgram {
            layer,
            program: crate::repair::REPAIR_PROGRAM.entry,
        });
    }
    let step = plan_repair(layer, entry, planning.linear)
        .map_err(|why| GpuFallback::Repair { layer, why })?;
    if step.resident.len() > REPAIR_PATCH_WORDS {
        return Err(GpuFallback::Repair {
            layer,
            why: format!(
                "its frozen patches need {} MiB of GPU storage at this stage, past the {} MiB a \
                 repair step may hold",
                (step.resident.len() * 4) >> 20,
                (REPAIR_PATCH_WORDS * 4) >> 20
            ),
        });
    }
    let levels: u64 = step
        .planes
        .iter()
        .filter(|plane| matches!(plane.size, GpuPlaneSize::Fixed { .. }))
        .map(|plane| plane.extent((0, 0), (0, 0)))
        .map(|(width, height)| u64::from(width) * u64::from(height) * 16)
        .sum();
    if step.passes.len() > REPAIR_PASSES_MAX {
        return Err(GpuFallback::Repair {
            layer,
            why: format!(
                "its corrections need {} GPU passes in {} groups of overlapping corrections, past \
                 the {REPAIR_PASSES_MAX} a repair step runs",
                step.passes.len(),
                groups(entry).len()
            ),
        });
    }
    if levels > REPAIR_LEVEL_BYTES {
        let largest = entry
            .operations()
            .iter()
            .zip(entry.bound())
            .filter(|(operation, _)| operation.kind == RepairKind::Heal)
            .max_by_key(|(_, op)| op.footprint.solve_rect().map_or(0, |rect| rect.pixels()))
            .map_or_else(String::new, |(operation, _)| operation.name());
        return Err(GpuFallback::Repair {
            layer,
            why: format!(
                "{largest}'s solve needs {} MiB of GPU planes, past the {} MiB a repair step may \
                 hold",
                levels >> 20,
                REPAIR_LEVEL_BYTES >> 20
            ),
        });
    }
    Ok(step)
}

/// What a repair step carries beside its passes: each operation's identity and regions at the
/// step's stage, for the change between two plans of one gesture, and the read set it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuRepair {
    /// Per operation in table order: a hash of its stored row, the pixels it may write and the two
    /// rectangles it reads.
    pub operations: Vec<GpuRepairOperation>,
    /// The step's read set, held in atlas planes of its own; `None` when no pass reads its input,
    /// a step of frozen patches alone or of operations that write nothing at this stage.
    pub atlas: Option<GpuReadAtlas>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuRepairOperation {
    pub key: u64,
    pub heal: bool,
    pub writes: Region,
    pub reads: [Region; 2],
}

/// A repair step's read set ([`RepairEntry::step_reads`]) packed into one atlas of
/// [`REPAIR_ATLAS_SIDE`] texels a side at most: the plane `plane` of the step holds its input
/// there, cut from the photograph's source by the surface before any of its passes reads it, and
/// its chained groups' states take the same placements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuReadAtlas {
    /// The step's plane that holds its input.
    pub plane: usize,
    /// The atlas's texels across and down.
    pub size: (u32, u32),
    /// Each rectangle of the stage, with the atlas texel its first pixel is held at.
    pub rects: Vec<(Region, [u32; 2])>,
}

/// The longest side, in texels, of a repair step's read atlas: the photo surface's device limit.
/// A read set that does not pack into one is drawn by the reference, named.
pub const REPAIR_ATLAS_SIDE: u32 = 8192;

/// What an atlas's sides are rounded up to, so the planes of a gesture's ticks keep their size
/// while its reads grow a little.
const ATLAS_ROUNDING: u32 = 256;

/// The side, in pixels, of the cells the read table indexes its rectangles by, of the stage and of
/// the atlas alike.
const READ_INDEX_CELL: u32 = 64;

/// Words of the read table before its rectangles' records, six words each.
const READ_HEADER_WORDS: usize = 10;

/// An atlas's size and each rectangle's placement in it.
pub(crate) type Packed = ((u32, u32), Vec<[u32; 2]>);

/// `rects` packed into one atlas: shelves of rectangles, tallest first, across a width near the
/// square root of their area, every placement a function of the sizes alone, so a source dragged
/// keeps every placement. The atlas's size, rounded up to [`ATLAS_ROUNDING`], and each rectangle's
/// placement in `rects`' order, or what does not fit within [`REPAIR_ATLAS_SIDE`].
pub(crate) fn pack(rects: &[Region]) -> Result<Packed, String> {
    let round = |side: u32| side.div_ceil(ATLAS_ROUNDING).max(1) * ATLAS_ROUNDING;
    let widest = rects.iter().map(|rect| rect.width).max().unwrap_or(1);
    let tallest = rects.iter().map(|rect| rect.height).max().unwrap_or(1);
    if widest > REPAIR_ATLAS_SIDE || tallest > REPAIR_ATLAS_SIDE {
        return Err(format!(
            "one rectangle it reads is {widest}x{tallest} pixels, past the {REPAIR_ATLAS_SIDE} a \
             side of one GPU atlas"
        ));
    }
    let area: u64 = rects.iter().map(|rect| rect.pixels()).sum();
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&index| {
        let rect = rects[index];
        (
            std::cmp::Reverse(rect.height),
            std::cmp::Reverse(rect.width),
            index,
        )
    });
    let shelve = |width: u32| -> (u32, Vec<[u32; 2]>) {
        let mut placed = vec![[0, 0]; rects.len()];
        let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
        for &index in &order {
            let rect = rects[index];
            if x > 0 && x + rect.width > width {
                (x, y, shelf) = (0, y + shelf, 0);
            }
            placed[index] = [x, y];
            x += rect.width;
            shelf = shelf.max(rect.height);
        }
        (y + shelf, placed)
    };
    let square = round(((area as f64).sqrt().ceil() as u32).max(widest)).min(REPAIR_ATLAS_SIDE);
    for width in [square, REPAIR_ATLAS_SIDE] {
        let width = width.max(widest);
        let (height, placed) = shelve(width);
        if height <= REPAIR_ATLAS_SIDE {
            let size = (
                round(width).min(REPAIR_ATLAS_SIDE),
                round(height).min(REPAIR_ATLAS_SIDE),
            );
            return Ok((size, placed));
        }
    }
    Err(format!(
        "the {} rectangles it reads, {} MP, do not pack into one GPU atlas of {REPAIR_ATLAS_SIDE} \
         texels a side",
        rects.len(),
        area.div_ceil(1_000_000)
    ))
}

/// The read table a repair step's block starts with (`repair.wgsl`, "The read set"): its header,
/// each rectangle's record, then an index of the stage's cells and one of the atlas's, each listing
/// the rectangles that meet a cell. `clamps` where the byte path clamps the step's input.
fn read_table(stage: Stage, atlas: &GpuReadAtlas, clamps: bool) -> Vec<u32> {
    let mut table = vec![0u32; READ_HEADER_WORDS];
    table[0] = atlas.rects.len() as u32;
    table[1] = u32::from(clamps);
    for (rect, at) in &atlas.rects {
        table.extend([rect.x0, rect.y0, rect.width, rect.height, at[0], at[1]]);
    }
    // One index: per cell of a `width x height` plane, the rectangles whose placement there meets it.
    let index = |table: &mut Vec<u32>, header: usize, (width, height): (u32, u32), atlas_side| {
        let columns = width.div_ceil(READ_INDEX_CELL).max(1);
        let rows = height.div_ceil(READ_INDEX_CELL).max(1);
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); (columns * rows) as usize];
        for (number, (rect, at)) in atlas.rects.iter().enumerate() {
            let (x0, y0) = if atlas_side {
                (at[0], at[1])
            } else {
                (rect.x0, rect.y0)
            };
            let (x1, y1) = (x0 + rect.width, y0 + rect.height);
            for row in y0 / READ_INDEX_CELL..=(y1 - 1) / READ_INDEX_CELL {
                for column in x0 / READ_INDEX_CELL..=(x1 - 1) / READ_INDEX_CELL {
                    if row < rows && column < columns {
                        lists[(row * columns + column) as usize].push(number as u32);
                    }
                }
            }
        }
        table[header] = READ_INDEX_CELL;
        table[header + 1] = columns;
        table[header + 2] = rows;
        table[header + 3] = table.len() as u32;
        let mut offset = (lists.len() + 1) as u32;
        let mut offsets = Vec::with_capacity(lists.len() + 1);
        for list in &lists {
            offsets.push(offset);
            offset += list.len() as u32;
        }
        offsets.push(offset);
        table.extend(offsets);
        for list in &lists {
            table.extend_from_slice(list);
        }
    };
    index(&mut table, 2, (stage.width, stage.height), false);
    index(&mut table, 6, atlas.size, true);
    table
}

fn meets(a: Region, b: Region) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.x0 < b.x1()
        && b.x0 < a.x1()
        && a.y0 < b.y1()
        && b.y0 < a.y1()
}

/// The ordered groups, each the operations, in table order, of one set of dispatches. Two
/// operations conflict when one writes what the other reads (its destination with Heal's ring, or
/// its source); an operation goes in the group after the latest one holding an earlier operation
/// it conflicts with, or the first. Operations that do not conflict commute, so every group reads
/// what the table order would give it, and the groups are as few as the longest chain of
/// conflicts: independent corrections anywhere in the table share one group.
/// `O(operations²)`.
pub(crate) fn groups(entry: &RepairEntry) -> Vec<Vec<usize>> {
    let regions = entry.regions();
    let conflict = |a: &crate::render::repair_entry::OperationRegions,
                    b: &crate::render::repair_entry::OperationRegions| {
        b.reads.iter().any(|read| meets(a.writes, *read))
            || a.reads.iter().any(|read| meets(b.writes, *read))
    };
    let mut level: Vec<usize> = Vec::with_capacity(regions.len());
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, op) in regions.iter().enumerate() {
        if op.writes.is_empty() {
            level.push(usize::MAX);
            continue;
        }
        let at = regions[..index]
            .iter()
            .zip(&level)
            .filter(|(earlier, level)| **level != usize::MAX && conflict(earlier, op))
            .map(|(_, level)| *level + 1)
            .max()
            .unwrap_or(0);
        level.push(at);
        if groups.len() <= at {
            groups.push(Vec::new());
        }
        groups[at].push(index);
    }
    if groups.is_empty() {
        // Every destination missed the stage: one group holds them all, and writes nothing.
        groups.push((0..regions.len()).collect());
    }
    groups
}

/// The repair boundary `entry` of layer `layer` as a spatial step of the plan, its input and output
/// clamped where the byte path quantizes ([module](self) docs), or why its read set does not pack
/// into one atlas.
pub(crate) fn plan_repair(
    layer: usize,
    entry: &RepairEntry,
    linear: bool,
) -> Result<GpuSpatial, String> {
    let bound = entry.bound();
    // A frozen patch reads nothing another operation of its group writes, and writes nothing one
    // reads, so it is drawn on its own right after the rest of its group.
    let mut phases: Vec<Phase> = Vec::new();
    for group in groups(entry) {
        let (patches, others): (Vec<usize>, Vec<usize>) = group
            .into_iter()
            .partition(|&index| entry.operations()[index].kind == RepairKind::Patch);
        if !others.is_empty() {
            phases.push(Phase::Group(others));
        }
        phases.extend(patches.into_iter().map(Phase::Patch));
    }
    let resident = patch_resident(entry);
    let state_format = if linear {
        GpuPlaneFormat::Quad
    } else {
        GpuPlaneFormat::Colour
    };
    // The read set, held in atlas planes of the step's own: its input, cut from the source, and
    // the states chained groups leave, every one at the same placements. A step whose every phase
    // is a patch drawn last reads no input and holds none.
    let reads_input = phases
        .iter()
        .enumerate()
        .any(|(number, phase)| matches!(phase, Phase::Group(_)) || number + 1 < phases.len());
    let rects = match (reads_input, entry.step_reads()) {
        (false, _) => Vec::new(),
        // Every operation writes nothing at this stage: their passes read nothing, and the atlas
        // holds one pixel for them to be bound to.
        (true, rects) if rects.is_empty() => vec![Region {
            x0: 0,
            y0: 0,
            width: 1,
            height: 1,
        }],
        (true, rects) => rects,
    };
    let atlas = if rects.is_empty() {
        None
    } else {
        let (size, placed) = pack(&rects)?;
        Some(GpuReadAtlas {
            plane: 0,
            size,
            rects: rects.into_iter().zip(placed).collect(),
        })
    };
    let atlas_plane = |atlas: &GpuReadAtlas| GpuPlane {
        format: state_format,
        size: GpuPlaneSize::Fixed {
            width: atlas.size.0,
            height: atlas.size.1,
        },
        scratch: false,
    };
    let mut words: Vec<u32> = Vec::new();
    let mut planes: Vec<GpuPlane> = Vec::new();
    let mut passes: Vec<GpuPass> = Vec::new();
    // The block: the read table, then each group's block and each patch's record.
    let mut block: Vec<u32> = match &atlas {
        Some(atlas) => read_table(entry.stage, atlas, !linear),
        None => Vec::new(),
    };
    // The input plane, then the states: plane 0, then 1 and 2.
    let input = atlas.as_ref().map(|atlas| {
        planes.push(atlas_plane(atlas));
        0
    });
    let states: Vec<usize> = match (&atlas, phases.len()) {
        (None, _) | (_, 0 | 1) => Vec::new(),
        (Some(_), 2) => vec![1],
        (Some(_), _) => vec![1, 2],
    };
    if let Some(atlas) = &atlas {
        for _ in &states {
            planes.push(atlas_plane(atlas));
        }
    }
    // The flags a pass reading plane `plane` at slot `slot` takes: 4, the plane is an atlas the
    // read table addresses; 16, it holds the step's input, which the table may clamp.
    let reading =
        |plane: usize, slot: u32| 4 | (slot << 8) | if Some(plane) == input { 16 } else { 0 };
    // The plane the phase being placed reads: the step's input, or a state.
    let mut state: Option<usize> = input;
    let mut apply = None;
    // An apply also binds the input plane last, unread, so the link keeps it across ticks: the
    // surface cuts it once for the plan's reads, not each tick.
    let keep = |mut planes: Vec<usize>| {
        planes.extend(input);
        planes
    };
    for (number, phase) in phases.iter().enumerate() {
        let last = number + 1 == phases.len();
        let next = |state: Option<usize>| match state {
            Some(plane) if plane == states[0] && states.len() > 1 => states[1],
            _ => states[0],
        };
        let group = match phase {
            Phase::Group(group) => group,
            Phase::Patch(index) => {
                let record = block.len() as u32;
                block.extend_from_slice(&resident.records[*index]);
                let words_at = words.len();
                let chained = state.filter(|plane| Some(*plane) != input);
                if last {
                    // `[-, -, -, record]`: the patch over the step's input, or over the state
                    // the phases before it left (bound first).
                    words.extend([0, 0, 0, record]);
                    apply = Some(match chained {
                        None => GpuApply {
                            function: "lf_repair_patch_apply",
                            planes: keep(Vec::new()),
                            words: words_at,
                            identity: false,
                        },
                        Some(plane) => GpuApply {
                            function: "lf_repair_patch_apply_over",
                            planes: keep(vec![plane]),
                            words: words_at,
                            identity: false,
                        },
                    });
                } else {
                    // `[-, -, flags, record]`: the patch over the plane at slot 0.
                    let read = state.expect("a chained patch reads the input plane");
                    words.extend([0, 0, reading(read, 0), record]);
                    let output = next(chained);
                    passes.push(GpuPass {
                        kernel: "lf_repair_patch_blend",
                        inputs: vec![read],
                        output,
                        words: words_at,
                        source: 0,
                        shape: GpuPassShape::Texels { span: [1, 1] },
                        reads_source: false,
                        unit: 0,
                    });
                    state = Some(output);
                }
                continue;
            }
        };
        let read = state.expect("a group reads the input plane");
        let ops: Vec<crate::repair::RepairOp> =
            group.iter().map(|&index| bound[index].clone()).collect();
        let described = crate::repair::gpu::describe(&ops, entry.stage, None);
        let offset = block.len() as u32;
        block.extend_from_slice(&described.block);
        let words_at = words.len();
        // Each four-word record: level, parity, flags, the group's block offset.
        words.extend_from_slice(&described.words);
        let planes_at = planes.len();
        for (index, &(width, height)) in described.planes.iter().enumerate() {
            planes.push(GpuPlane {
                format: GpuPlaneFormat::Quad,
                size: GpuPlaneSize::Fixed { width, height },
                // The last group's composite is what the apply reads.
                scratch: !(last && index == described.apply_plane),
            });
        }
        // Every record names the group's block offset; a pass that reads the group's input reads
        // the input plane or the state, bound after its own inputs.
        let mut record = |at: usize| {
            words[words_at + at + 3] = offset;
        };
        for pass in &described.passes {
            record(pass.words);
        }
        record(described.apply_words);
        for pass in &described.passes {
            let mut inputs: Vec<usize> =
                pass.inputs.iter().map(|&plane| planes_at + plane).collect();
            if pass.reads_source {
                words[words_at + pass.words + 2] |= reading(read, inputs.len() as u32);
                inputs.push(read);
            }
            passes.push(GpuPass {
                kernel: pass.kernel,
                inputs,
                output: planes_at + pass.output,
                words: words_at + pass.words,
                source: 0,
                shape: GpuPassShape::Texels { span: [1, 1] },
                reads_source: false,
                unit: 0,
            });
        }
        let composite = planes_at + described.apply_plane;
        let chained = state.filter(|plane| Some(*plane) != input);
        if last {
            apply = Some(match chained {
                None => GpuApply {
                    function: "lf_repair_apply",
                    planes: keep(vec![composite]),
                    words: words_at + described.apply_words,
                    identity: false,
                },
                Some(plane) => GpuApply {
                    function: "lf_repair_apply_over",
                    planes: keep(vec![plane, composite]),
                    words: words_at + described.apply_words,
                    identity: false,
                },
            });
        } else {
            // The blend into the next state over the atlas: words `[-, -, flags, offset,
            // composite slot]`, the plane it blends over at slot 1.
            let output = next(chained);
            let at = words.len();
            words.extend([0, 0, reading(read, 1), offset, 0]);
            passes.push(GpuPass {
                kernel: "lf_repair_blend",
                inputs: vec![composite, read],
                output,
                words: at,
                source: 0,
                shape: GpuPassShape::Texels { span: [1, 1] },
                reads_source: false,
                unit: 0,
            });
            state = Some(output);
        }
    }
    let apply = apply.expect("a repair layer of at least one operation");
    let operations = entry
        .operations()
        .iter()
        .zip(entry.regions())
        .map(|(operation, regions)| GpuRepairOperation {
            key: {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::hash::DefaultHasher::new();
                (
                    operation.id,
                    operation.kind == RepairKind::Heal,
                    &operation.points[..],
                    operation.radius,
                    operation.feather,
                    operation.opacity,
                    operation.offset,
                )
                    .hash(&mut hasher);
                if let Some(patch) = &operation.patch {
                    let full = patch.source.full;
                    (
                        &patch.source.artifact.id,
                        patch.source.rect,
                        full.width,
                        full.height,
                    )
                        .hash(&mut hasher);
                }
                hasher.finish()
            },
            heal: operation.kind == RepairKind::Heal,
            writes: regions.writes,
            reads: regions.reads,
        })
        .collect();
    Ok(GpuSpatial {
        layer,
        program: &crate::repair::REPAIR_PROGRAM,
        words,
        planes,
        passes,
        applies: vec![apply],
        clamps: !linear,
        mask: None,
        after: Vec::new(),
        halos: Vec::new(),
        light: None,
        block: Some(block.into()),
        repair: Some(Arc::new(GpuRepair { operations, atlas })),
        resident: resident.blocks,
    })
}

/// One set of a repair step's dispatches: a group of Clone and Heal operations drawn through the
/// atlas, or one frozen patch drawn straight from its words.
enum Phase {
    Group(Vec<usize>),
    Patch(usize),
}

/// The most words a repair step's frozen patches may take on either GPU device: 64 MiB, the largest
/// one patch's bytes. A layer whose patches need more at the stage it is drawn at is drawn by the
/// reference, named.
pub const REPAIR_PATCH_WORDS: usize = 16 * 1024 * 1024;

/// A repair step's frozen patches on the GPU: the words each is drawn from at the step's stage
/// ([`crate::repair::patch::drawn`]), each distinct allocation once, and per operation of the
/// table its record, empty for a Clone or Heal.
struct PatchResident {
    blocks: super::GpuResident,
    records: Vec<Vec<u32>>,
}

/// The words of every frozen patch of `entry` at its stage, shared with the reference's frames and
/// earlier plans through the drawn-patch cache, and each patch's record:
///
/// `0..4` the stage pixels it may write `[x0, y0, x1, y1)`, `4` 1 when its words are reduced
/// means (four `f32` a pixel: `P̄`, `Ā`) and 0 when they are its own half floats (two words a
/// pixel), `5` its words' offset from the first resident word, `6` their row stride in pixels,
/// `7, 8` the stage pixel of their first, `9` `opacity / 100` (`f32`), `10` every resident word,
/// which the step's block base is past.
fn patch_resident(entry: &RepairEntry) -> PatchResident {
    use std::hash::{Hash, Hasher};
    let stage = entry.stage;
    let mut blocks: Vec<Arc<[u32]>> = Vec::new();
    let mut hasher = std::hash::DefaultHasher::new();
    type Placed = (usize, Option<(u32, Arc<crate::repair::patch::Drawn>)>);
    let mut placed: Vec<Placed> = Vec::new();
    let mut offset = 0u32;
    for (index, operation) in entry.operations().iter().enumerate() {
        let Some(patch) = &operation.patch else {
            placed.push((index, None));
            continue;
        };
        let drawn = crate::repair::patch::drawn(&patch.source, stage);
        let at = match blocks
            .iter()
            .position(|held| Arc::ptr_eq(held, &drawn.words))
        {
            Some(found) => blocks[..found].iter().map(|block| block.len() as u32).sum(),
            None => {
                let at = offset;
                offset += drawn.words.len() as u32;
                let full = patch.source.full;
                (
                    &patch.source.artifact.id,
                    patch.source.rect,
                    [full.width, full.height, stage.width, stage.height],
                )
                    .hash(&mut hasher);
                blocks.push(drawn.words.clone());
                at
            }
        };
        placed.push((index, Some((at, drawn))));
    }
    let total = offset;
    let records = placed
        .into_iter()
        .map(|(index, drawn)| {
            let Some((at, drawn)) = drawn else {
                return Vec::new();
            };
            let operation = &entry.operations()[index];
            let source = &operation.patch.as_ref().expect("a patch operation").source;
            let region = drawn.region;
            let (stride, origin) = if drawn.reduced {
                (region.width, [region.x0, region.y0])
            } else {
                (source.rect[2], [source.rect[0], source.rect[1]])
            };
            vec![
                region.x0,
                region.y0,
                region.x1(),
                region.y1(),
                u32::from(drawn.reduced),
                at,
                stride,
                origin[0],
                origin[1],
                (f32::from(operation.opacity.min(100)) / 100.0).to_bits(),
                total,
            ]
        })
        .collect();
    PatchResident {
        blocks: super::GpuResident {
            blocks,
            key: hasher.finish(),
        },
        records,
    }
}

/// The plans a Corrections gesture on a stack draws, for the desktop to warm before it begins
/// (`docs/design/corrections.md`, "Warm-up"): the stack's repair links as they stand, a new top
/// Heal and a new top Clone drafted over them, and an existing operation's update drag — or, on a
/// stack that holds no repair layer yet, the first Heal and Clone at the place a repair layer would
/// take. `compiled` is the stack compiled for `request`, a plan from the source; the synthetic
/// operations are a small spot in the middle of the frame with a source beside it. A pass pipeline
/// is its kernel, inputs and format, so these warm every one a repair tick compiles; a stack whose
/// registry declares no repair effect has none. `O(layers + operations)`, no pixel read.
pub(crate) fn warm_plans(
    registry: &crate::ModuleRegistry,
    recipe: &crate::Recipe,
    compiled: &crate::render::Compiled,
    request: super::GpuPlanRequest,
) -> Result<Vec<super::GpuPlan>, crate::Error> {
    use crate::{
        EffectStage,
        modules::{RepairOperation, RepairTable},
        render::Entry,
    };
    let current = compiled
        .segments
        .iter()
        .find_map(|segment| segment.entry.as_ref().and_then(Entry::repair_entry))
        .map(|entry| entry.table.clone());
    // On a stack without one, the place a repair layer would take, when the registry declares one.
    let place = match &current {
        Some(_) => None,
        None => {
            let effect = registry.providers().find_map(|provider| {
                provider
                    .descriptor()
                    .effects
                    .iter()
                    .find(|effect| {
                        effect.stage == EffectStage::Repair && !provider.descriptor().developer
                    })
                    .map(|effect| effect.id.clone())
            });
            match effect {
                Some(effect) => Some(registry.insertion_index_for(&recipe.layers, &effect)),
                None => return Ok(Vec::new()),
            }
        }
    };
    let rows: Vec<RepairOperation> = current
        .as_ref()
        .map(|table| table.operations().to_vec())
        .unwrap_or_default();
    let next = rows
        .iter()
        .map(|row| row.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let spot = |id: u32, kind: RepairKind| RepairOperation {
        id,
        kind,
        points: Arc::from([[8192, 8192]]),
        radius: 164,
        feather: 50,
        opacity: 100,
        offset: [1638, 0],
        patch: None,
    };
    let mut tables: Vec<Vec<RepairOperation>> = Vec::new();
    if !rows.is_empty() {
        tables.push(rows.clone());
        let mut moved = rows.clone();
        if let Some(last) = moved.last_mut() {
            last.offset[1] = if last.offset[1] > 0 {
                last.offset[1] - 1
            } else {
                last.offset[1] + 1
            };
        }
        tables.push(moved);
    }
    if rows.len() < crate::modules::MAX_REPAIR_OPERATIONS {
        for kind in [RepairKind::Heal, RepairKind::Clone] {
            let mut drafted = rows.clone();
            drafted.push(spot(next, kind));
            tables.push(drafted);
        }
    }
    let mut plans = Vec::new();
    for rows in tables {
        let table = RepairTable::new(rows)?;
        let Some(modified) = with_table(compiled, table, request.stage, place) else {
            continue;
        };
        let answer = modified.gpu_plan(
            0,
            request.stage,
            None,
            Planning {
                qualifying: request.qualifying,
                linear: request.linear,
                source: true,
            },
        )?;
        if let super::GpuAnswer::Plan(plan) =
            super::plan::with_lights(registry, recipe, request, &modified, answer)?
        {
            plans.push(*plan);
        }
    }
    Ok(plans)
}

/// `compiled` with its repair boundary's table replaced by `table`, or — `place`, the layer a
/// repair layer would take — with a repair boundary of `table` opened there over `stage`, the
/// leading segment's operations from that layer on carried into the new segment. `None` when the
/// place lies outside the leading segment, where no repair layer goes.
fn with_table(
    compiled: &crate::render::Compiled,
    table: crate::modules::RepairTable,
    stage: Stage,
    place: Option<usize>,
) -> Option<crate::render::Compiled> {
    use crate::{
        modules::{ExactGeometry, Processing},
        render::{Entry, Segment},
    };
    let mut compiled = compiled.clone();
    if let Some(segment) = compiled
        .segments
        .iter_mut()
        .find(|segment| matches!(segment.entry, Some(Entry::Repair(_))))
    {
        let Some(Entry::Repair(entry)) = &segment.entry else {
            unreachable!("a repair entry")
        };
        let bound = entry.stage;
        segment.entry = Some(Entry::repair(table, bound));
        return Some(compiled);
    }
    let layer = place?;
    let first = compiled.segments.first()?;
    let (segment, position) = match compiled.layers.get(layer) {
        Some(&start) => start,
        None => (
            compiled.segments.len() - 1,
            compiled.segments.last()?.operations.len(),
        ),
    };
    if segment != 0 || first.entry.is_some() {
        return None;
    }
    let moved: Vec<Processing> = first.operations[position..].to_vec();
    let kept: Vec<Processing> = first.operations[..position].to_vec();
    if kept
        .iter()
        .any(|operation| !matches!(operation, Processing::Color(_)))
    {
        return None;
    }
    let mut geometry = ExactGeometry::identity(stage.width, stage.height);
    for operation in &moved {
        if let Processing::ExactGeometry(step) = operation {
            geometry = geometry.then(*step);
        }
    }
    let old = compiled.segments[0].clone();
    let opened = Segment {
        entry: Some(Entry::repair(table, stage)),
        has_color: moved
            .iter()
            .any(|operation| matches!(operation, Processing::Color(_))),
        operations: moved,
        geometry,
        width: old.width,
        height: old.height,
        output_origin: old.output_origin,
    };
    let leading = Segment {
        has_color: !kept.is_empty(),
        operations: kept,
        geometry: ExactGeometry::identity(stage.width, stage.height),
        width: stage.width,
        height: stage.height,
        output_origin: (0, 0),
        entry: None,
    };
    compiled.segments[0] = leading;
    compiled.segments.insert(1, opened);
    let mut layers: Vec<(usize, usize)> = compiled
        .layers
        .iter()
        .map(|&(segment, at)| match segment {
            0 if at >= position => (1, at - position),
            0 => (0, at),
            later => (later + 1, at),
        })
        .collect();
    layers.insert(layer.min(layers.len()), (0, position));
    compiled.layers = layers.into_boxed_slice();
    Some(compiled)
}
