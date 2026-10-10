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
//! - **Reads.** The step's window holds every pixel its operations read for the output it is
//!   asked for (`RepairEntry::window_reads`); an operation no output pixel needs may compute values
//!   outside that set from clamped reads, which nothing reads.
//!
//! Planned on the catalog owner from the bound table: `O(operations + segments × nearby tiles +
//! tiles)` for the groups' blocks, no pixel read.
use super::{
    GpuFallback, GpuPass, GpuPassShape, GpuPlane, GpuPlaneFormat, GpuPlaneSize, GpuSpatial,
    plan::Planning,
    spatial::GpuApply,
};
use crate::{
    modules::{Region, RepairKind, Stage},
    render::repair_entry::RepairEntry,
};
use std::sync::Arc;

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
    let step = plan_repair(layer, entry, planning.linear);
    let levels: u64 = step
        .planes
        .iter()
        .filter(|plane| matches!(plane.size, GpuPlaneSize::Fixed { .. }))
        .map(|plane| plane.extent((0, 0), (0, 0)))
        .map(|(width, height)| u64::from(width) * u64::from(height) * 16)
        .sum();
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
/// step's stage, for the change between two plans of one gesture, and how far its reads reach
/// beyond its writes, for the staged sweeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuRepair {
    /// Per operation in table order: a hash of its stored row, the pixels it may write and the two
    /// rectangles it reads.
    pub operations: Vec<GpuRepairOperation>,
    /// The farthest any operation reads beyond its destination, in pixels of its stage.
    pub reach: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuRepairOperation {
    pub key: u64,
    pub heal: bool,
    pub writes: Region,
    pub reads: [Region; 2],
}

fn meets(a: Region, b: Region) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.x0 < b.x1()
        && b.x0 < a.x1()
        && a.y0 < b.y1()
        && b.y0 < a.y1()
}

/// The ordered groups: runs of consecutive operations none of whose reads meets an earlier
/// destination of the run.
pub(crate) fn groups(entry: &RepairEntry) -> Vec<std::ops::Range<usize>> {
    let regions = entry.regions();
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (index, op) in regions.iter().enumerate() {
        let joins = groups.last().is_some_and(|group| {
            group.clone().all(|earlier| {
                let writes = regions[earlier].writes;
                !meets(writes, op.reads[0]) && !meets(writes, op.reads[1])
            })
        });
        match groups.last_mut() {
            Some(group) if joins => group.end = index + 1,
            _ => groups.push(index..index + 1),
        }
    }
    groups
}

/// The repair boundary `entry` of layer `layer` as a spatial step of the plan, its input and output
/// clamped where the byte path quantizes ([module](self) docs).
pub(crate) fn plan_repair(layer: usize, entry: &RepairEntry, linear: bool) -> GpuSpatial {
    let bound = entry.bound();
    let groups = groups(entry);
    let state_format = if linear {
        GpuPlaneFormat::Quad
    } else {
        GpuPlaneFormat::Colour
    };
    let mut words: Vec<u32> = Vec::new();
    let mut planes: Vec<GpuPlane> = Vec::new();
    let mut passes: Vec<GpuPass> = Vec::new();
    let mut block: Vec<u32> = Vec::new();
    let states: Vec<usize> = match groups.len() {
        0 | 1 => Vec::new(),
        2 => vec![0],
        _ => vec![0, 1],
    };
    for _ in &states {
        planes.push(GpuPlane {
            format: state_format,
            size: GpuPlaneSize::Reduced(1),
            scratch: false,
        });
    }
    // The state the group being placed reads: the step's input, or a state plane.
    let mut state: Option<usize> = None;
    let mut apply = None;
    for (number, group) in groups.iter().enumerate() {
        let described = crate::repair::gpu::describe(&bound[group.clone()], entry.stage, None);
        let offset = block.len() as u32;
        block.extend_from_slice(&described.block);
        let words_at = words.len();
        // Each four-word record: level, parity, flags, the group's block offset.
        words.extend_from_slice(&described.words);
        let planes_at = planes.len();
        let last = number + 1 == groups.len();
        for (index, &(width, height)) in described.planes.iter().enumerate() {
            planes.push(GpuPlane {
                format: GpuPlaneFormat::Quad,
                size: GpuPlaneSize::Fixed { width, height },
                // The last group's composite is what the apply reads.
                scratch: !(last && index == described.apply_plane),
            });
        }
        // Every record names the group's block offset; a pass that reads the input reads the
        // state plane, when there is one, bound after its own inputs.
        let mut record = |at: usize, flags: u32| {
            words[words_at + at + 2] |= flags;
            words[words_at + at + 3] = offset;
        };
        for pass in &described.passes {
            record(pass.words, 0);
        }
        record(described.apply_words, 0);
        for pass in &described.passes {
            let mut inputs: Vec<usize> = pass.inputs.iter().map(|&plane| planes_at + plane).collect();
            let reads_source = match (pass.reads_source, state) {
                (true, Some(plane)) => {
                    let slot = inputs.len() as u32;
                    words[words_at + pass.words + 2] |= 4 | (slot << 8);
                    inputs.push(plane);
                    false
                }
                (reads, _) => reads,
            };
            passes.push(GpuPass {
                kernel: pass.kernel,
                inputs,
                output: planes_at + pass.output,
                words: words_at + pass.words,
                source: 0,
                shape: GpuPassShape::Texels { span: [1, 1] },
                reads_source,
                unit: 0,
            });
        }
        let composite = planes_at + described.apply_plane;
        if last {
            apply = Some(match state {
                None => GpuApply {
                    function: "lf_repair_apply",
                    planes: vec![composite],
                    words: words_at + described.apply_words,
                    identity: false,
                },
                Some(plane) => GpuApply {
                    function: "lf_repair_apply_over",
                    planes: vec![plane, composite],
                    words: words_at + described.apply_words,
                    identity: false,
                },
            });
        } else {
            // The blend into the next state: words `[-, -, flags, offset, composite slot]`.
            let output = match state {
                Some(plane) if plane == states[0] && states.len() > 1 => states[1],
                _ => states[0],
            };
            let mut inputs = vec![composite];
            let mut flags = 0;
            let mut reads_source = true;
            if let Some(plane) = state {
                flags = 4 | (1 << 8);
                inputs.push(plane);
                reads_source = false;
            }
            let at = words.len();
            words.extend([0, 0, flags, offset, 0]);
            passes.push(GpuPass {
                kernel: "lf_repair_blend",
                inputs,
                output,
                words: at,
                source: 0,
                shape: GpuPassShape::Texels { span: [1, 1] },
                reads_source,
                unit: 0,
            });
            state = Some(output);
        }
    }
    let apply = apply.expect("a repair layer of at least one operation");
    let reach = entry
        .regions()
        .iter()
        .map(|regions| {
            let dx = regions.delta[0].unsigned_abs();
            let dy = regions.delta[1].unsigned_abs();
            u32::try_from(dx.max(dy)).unwrap_or(u32::MAX).saturating_add(1)
        })
        .max()
        .unwrap_or(0);
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
                hasher.finish()
            },
            heal: operation.kind == RepairKind::Heal,
            writes: regions.writes,
            reads: regions.reads,
        })
        .collect();
    GpuSpatial {
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
        repair: Some(Arc::new(GpuRepair { operations, reach })),
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

