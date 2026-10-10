//! A repair layer on the GPU (`docs/design/corrections.md`, "Rendering (TASK-003)"): the repair
//! boundary as one spatial step of the plan, its program `repair/repair.wgsl`, its operation table
//! in the step's storage block.
//!
//! - **Groups.** The operations evaluate in table order, grouped into ordered passes: consecutive
//!   Clone operations share one pass until one reads (its destination, its source) a pixel an
//!   earlier operation of the group writes; each Heal is a group of its own. A Clone group is one
//!   pass over the window from one state plane into the next, so independent dust spots cost one
//!   dispatch; a Heal group is its solve's passes over planes of its solve rectangle, then one
//!   composite pass into the next state plane.
//! - **State.** The first group reads the step's input; groups then alternate between two state
//!   planes of the window, and the last writes the plane the step's one apply reads. Planes are
//!   `rgba16float` on the byte path, whose boundary is half floats, and `rgba32float` on the linear
//!   path.
//! - **Heal.** Its levels are planes of fixed sizes, the solve rectangle halved rounding up to one
//!   cell, each level two value planes (a red-black half iteration reads one and writes the other)
//!   and a right-hand side; the planes of a level are shared by every Heal, sized for the largest.
//!   The passes are the CPU's schedule (`repair/heal.rs`): classes, the start's climb and descent,
//!   three V(2, 2) cycles, the composite. A level past the CPU's last is inert: it holds no unknown,
//!   so it neither starts nor corrects one.
//! - **Reads.** The step's window holds every pixel its operations read for the output it is
//!   asked for (`RepairEntry::window_reads`); an operation no output pixel needs may compute values
//!   outside that set from clamped reads, which nothing reads.
//!
//! Planned on the catalog owner from the bound table: `O(operations · cells + segments)` for the
//! block's indexes, no pixel read.
use super::{
    GpuFallback, GpuPass, GpuPassShape, GpuPlane, GpuPlaneFormat, GpuPlaneSize, GpuSpatial,
    plan::Planning,
    program::{GpuProgram, GpuProgramKind},
    spatial::GpuApply,
};
use crate::{
    modules::{Region, RepairKind, Stage},
    render::repair_entry::RepairEntry,
};
use std::sync::Arc;

/// The repair program: Clone, Heal and their coverage under the spatial convention.
pub static REPAIR_PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_repair",
    source: include_str!("../../repair/repair.wgsl"),
    kind: GpuProgramKind::Spatial,
    words: 0,
    enabled: true,
};

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
    if !REPAIR_PROGRAM.enabled && !planning.qualifying {
        return Err(GpuFallback::DisabledProgram {
            layer,
            program: REPAIR_PROGRAM.entry,
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

/// One operation record's words in the block.
const RECORD_WORDS: u32 = 20;

/// A level plane's sides are rounded up to a multiple of this, so a stroke that grows by a pixel
/// keeps its planes, and the step its pipelines.
const LEVEL_GRANULE: u32 = 32;

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

/// The ordered groups: ranges of the table, a Heal alone, a Clone joining the group before it
/// while it reads nothing that group writes.
fn groups(entry: &RepairEntry) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (index, (operation, regions)) in entry
        .operations()
        .iter()
        .zip(entry.regions())
        .enumerate()
    {
        let joins = operation.kind == RepairKind::Clone
            && groups.last().is_some_and(|group| {
                group.end == index
                    && entry.operations()[group.start].kind == RepairKind::Clone
                    && group.clone().all(|earlier| {
                        let writes = entry.regions()[earlier].writes;
                        !meets(writes, regions.reads[0]) && !meets(writes, regions.reads[1])
                    })
            });
        if joins {
            groups.last_mut().expect("a group").end = index + 1;
        } else {
            groups.push(index..index + 1);
        }
    }
    groups
}

/// The storage block: header, records, then each operation's index and segments in pixels.
fn block(entry: &RepairEntry) -> Arc<[u32]> {
    let bound = entry.bound();
    let stage = entry.stage;
    let h = f64::from(stage.height);
    let count = bound.len() as u32;
    let mut words: Vec<u32> = vec![count, stage.width, stage.height];
    let records = words.len() as u32;
    words.extend(std::iter::repeat_n(0, bound.len()));
    let mut tails: Vec<u32> = Vec::new();
    let mut records_words: Vec<u32> = Vec::with_capacity(bound.len() * RECORD_WORDS as usize);
    // Records first, tails after; offsets are fixed once the records' extent is known.
    let tail_start = records + count + count * RECORD_WORDS;
    for (index, op) in bound.iter().enumerate() {
        words[(records + index as u32) as usize] = records + count + index as u32 * RECORD_WORDS;
        let footprint = &op.footprint;
        let support = footprint.support();
        let (cell, columns, offsets, entries) = footprint.index();
        let rows = if support.is_empty() {
            0
        } else {
            support.height.div_ceil(cell.max(1))
        };
        let offsets_at = tail_start + tails.len() as u32;
        if offsets.is_empty() {
            tails.extend([0, 0]);
        } else {
            tails.extend_from_slice(offsets);
        }
        let entries_at = tail_start + tails.len() as u32;
        tails.extend_from_slice(entries);
        let segments_at = tail_start + tails.len() as u32;
        for segment in footprint.segments_ref() {
            tails.extend(
                [
                    segment.ax * h,
                    segment.ay * h,
                    segment.ex * h,
                    segment.ey * h,
                    segment.len2 * h * h,
                ]
                .map(|value| (value as f32).to_bits()),
            );
        }
        let solve = if op.kind == RepairKind::Heal {
            footprint.solve_rect().unwrap_or(Region::EMPTY)
        } else {
            Region::EMPTY
        };
        let (x1, y1) = if support.is_empty() {
            (0, 0)
        } else {
            (support.x1(), support.y1())
        };
        records_words.extend([
            u32::from(op.kind == RepairKind::Heal),
            ((footprint.radius() * h) as f32).to_bits(),
            ((footprint.band() * h) as f32).to_bits(),
            (footprint.amount() as f32).to_bits(),
            (op.delta[0] as i32) as u32,
            (op.delta[1] as i32) as u32,
            support.x0,
            support.y0,
            x1,
            y1,
            cell.max(1),
            columns,
            rows,
            offsets_at,
            entries_at,
            segments_at,
            solve.x0,
            solve.y0,
            solve.width,
            solve.height,
        ]);
    }
    words.extend(records_words);
    debug_assert_eq!(words.len() as u32, tail_start);
    words.extend(tails);
    words.into()
}

/// Every kernel and the apply a repair step names, for the core's WGSL validation, which calls
/// each.
#[cfg(test)]
pub(crate) fn functions() -> (Vec<&'static str>, Vec<&'static str>) {
    (
        vec![
            "lf_repair_clone",
            "lf_repair_heal_setup",
            "lf_repair_heal_coarse",
            "lf_repair_heal_descend",
            "lf_repair_heal_sweep",
            "lf_repair_heal_clear",
            "lf_repair_heal_restrict",
            "lf_repair_heal_prolong",
            "lf_repair_heal_composite",
        ],
        vec!["lf_repair_apply"],
    )
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
                    .find(|effect| effect.stage == EffectStage::Repair && !provider.descriptor().developer)
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
    let next = rows.iter().map(|row| row.id).max().unwrap_or(0).saturating_add(1);
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
        None => (compiled.segments.len() - 1, compiled.segments.last()?.operations.len()),
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

/// The level sizes of a solve rectangle of `width × height` cells: halved rounding up until one
/// cell remains.
fn level_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    let mut sizes = vec![(width.max(1), height.max(1))];
    while let Some(&(w, h)) = sizes.last()
        && (w > 1 || h > 1)
    {
        sizes.push((w.div_ceil(2), h.div_ceil(2)));
    }
    sizes
}

/// What the step's passes and planes are built from.
struct Builder {
    words: Vec<u32>,
    planes: Vec<GpuPlane>,
    passes: Vec<GpuPass>,
}

impl Builder {
    fn plane(&mut self, format: GpuPlaneFormat, size: GpuPlaneSize, scratch: bool) -> usize {
        self.planes.push(GpuPlane {
            format,
            size,
            scratch,
        });
        self.planes.len() - 1
    }

    fn pass(&mut self, kernel: &'static str, inputs: Vec<usize>, output: usize, words: &[u32]) {
        let reads_source = words.first() == Some(&1);
        self.passes.push(GpuPass {
            kernel,
            inputs,
            output,
            words: self.words.len(),
            source: 0,
            shape: GpuPassShape::Texels { span: [1, 1] },
            reads_source,
            unit: 0,
        });
        self.words.extend_from_slice(words);
    }
}

/// The state a group reads: the step's input, or a state plane.
#[derive(Clone, Copy)]
enum State {
    Input,
    Plane(usize),
}

impl State {
    /// The input word, and the planes a pass binds before its own.
    fn read(self) -> (u32, Vec<usize>) {
        match self {
            Self::Input => (1, Vec::new()),
            Self::Plane(plane) => (0, vec![plane]),
        }
    }
}

/// The repair boundary `entry` of layer `layer` as a spatial step of the plan, its input and output
/// clamped where the byte path quantizes (`clamps`), its planes in `format`.
pub(crate) fn plan_repair(layer: usize, entry: &RepairEntry, linear: bool) -> GpuSpatial {
    let format = if linear {
        GpuPlaneFormat::Quad
    } else {
        GpuPlaneFormat::Colour
    };
    let groups = groups(entry);
    // The apply's word, first, so every pass's words follow it.
    let mut builder = Builder {
        words: vec![0],
        planes: Vec::new(),
        passes: Vec::new(),
    };
    let full = GpuPlaneSize::Reduced(1);
    // The level planes, sized for the largest Heal's level.
    let heals: Vec<Vec<(u32, u32)>> = entry
        .operations()
        .iter()
        .enumerate()
        .filter(|(_, operation)| operation.kind == RepairKind::Heal)
        .map(|(index, _)| {
            let solve = entry.bound()[index]
                .footprint
                .solve_rect()
                .unwrap_or(Region::EMPTY);
            level_sizes(solve.width, solve.height)
        })
        .collect();
    let depth = heals.iter().map(Vec::len).max().unwrap_or(0);
    let granule = |side: u32| side.div_ceil(LEVEL_GRANULE) * LEVEL_GRANULE;
    let levels: Vec<([usize; 2], usize)> = (0..depth)
        .map(|level| {
            let (w, h) = heals
                .iter()
                .filter_map(|sizes| sizes.get(level))
                .fold((1, 1), |(w, h), &(sw, sh)| (w.max(sw), h.max(sh)));
            let size = GpuPlaneSize::Fixed {
                width: granule(w),
                height: granule(h),
            };
            let quad = GpuPlaneFormat::Quad;
            (
                [
                    builder.plane(quad, size, true),
                    builder.plane(quad, size, true),
                ],
                builder.plane(quad, size, true),
            )
        })
        .collect();
    let states = [
        builder.plane(format, full, true),
        builder.plane(format, full, true),
    ];
    let last = builder.plane(format, full, false);
    let mut state = State::Input;
    for (number, group) in groups.iter().enumerate() {
        let output = if number + 1 == groups.len() {
            last
        } else {
            match state {
                State::Plane(plane) if plane == states[0] => states[1],
                _ => states[0],
            }
        };
        let (flag, mut inputs) = state.read();
        let first = group.start;
        if entry.operations()[first].kind == RepairKind::Clone {
            builder.pass(
                "lf_repair_clone",
                inputs,
                output,
                &[flag, first as u32, group.len() as u32],
            );
        } else {
            let solve = entry.bound()[first]
                .footprint
                .solve_rect()
                .unwrap_or(Region::EMPTY);
            let sizes = level_sizes(solve.width, solve.height);
            heal_passes(&mut builder, &levels, &sizes, first as u32, state);
            let finest = builder
                .passes
                .last()
                .map(|pass| pass.output)
                .expect("a heal's passes");
            let (w, h) = sizes[0];
            let slot = inputs.len() as u32;
            inputs.push(finest);
            builder.pass(
                "lf_repair_heal_composite",
                inputs,
                output,
                &[flag, first as u32, w, h, slot],
            );
        }
        state = State::Plane(output);
    }
    let reach = entry
        .regions()
        .iter()
        .map(|regions| {
            let ring = 1u32;
            let dx = regions.delta[0].unsigned_abs();
            let dy = regions.delta[1].unsigned_abs();
            u32::try_from(dx.max(dy)).unwrap_or(u32::MAX).saturating_add(ring)
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
        program: &REPAIR_PROGRAM,
        words: builder.words,
        planes: builder.planes,
        passes: builder.passes,
        applies: vec![GpuApply {
            function: "lf_repair_apply",
            planes: vec![last],
            words: 0,
            identity: false,
        }],
        clamps: !linear,
        mask: None,
        after: Vec::new(),
        halos: Vec::new(),
        light: None,
        block: Some(block(entry)),
        repair: Some(Arc::new(GpuRepair { operations, reach })),
    }
}

/// One Heal's solve over the level planes: classes and the boundary difference, the start's climb
/// and descent, three V(2, 2) cycles. Its finest level's current plane is the last pass's output.
fn heal_passes(
    builder: &mut Builder,
    levels: &[([usize; 2], usize)],
    sizes: &[(u32, u32)],
    op: u32,
    state: State,
) {
    let depth = sizes.len();
    // Which of each level's two value planes holds its current values.
    let mut current = vec![0usize; depth];
    let value = |levels: &[([usize; 2], usize)], current: &[usize], level: usize| {
        levels[level].0[current[level]]
    };
    let (flag, inputs) = state.read();
    let (w, h) = sizes[0];
    builder.pass(
        "lf_repair_heal_setup",
        inputs,
        levels[0].0[0],
        &[flag, op, w, h],
    );
    for level in 1..depth {
        let (w, h) = sizes[level];
        let (fw, fh) = sizes[level - 1];
        builder.pass(
            "lf_repair_heal_coarse",
            vec![value(levels, &current, level - 1)],
            levels[level].0[0],
            &[0, op, w, h, fw, fh],
        );
    }
    for level in (0..depth.saturating_sub(1)).rev() {
        let (w, h) = sizes[level];
        let (cw, ch) = sizes[level + 1];
        let next = 1 - current[level];
        builder.pass(
            "lf_repair_heal_descend",
            vec![
                value(levels, &current, level),
                value(levels, &current, level + 1),
            ],
            levels[level].0[next],
            &[0, op, w, h, cw, ch],
        );
        current[level] = next;
    }
    for _ in 0..3 {
        v_cycle(builder, levels, sizes, op, &mut current, 0);
    }
    // The finest level's current values are the last pass's output when the last pass wrote it;
    // make it so by a final copy if a coarser pass ran last (it never does: post sweeps end a
    // cycle at level 0).
    debug_assert_eq!(
        builder.passes.last().map(|pass| pass.output),
        Some(levels[0].0[current[0]])
    );
}

fn v_cycle(
    builder: &mut Builder,
    levels: &[([usize; 2], usize)],
    sizes: &[(u32, u32)],
    op: u32,
    current: &mut [usize],
    level: usize,
) {
    let (w, h) = sizes[level];
    let has_rhs = u32::from(level > 0);
    let sweeps = |builder: &mut Builder, current: &mut [usize]| {
        for _ in 0..2 {
            for parity in 0..2u32 {
                let mut inputs = vec![levels[level].0[current[level]]];
                if level > 0 {
                    inputs.push(levels[level].1);
                }
                let next = 1 - current[level];
                builder.pass(
                    "lf_repair_heal_sweep",
                    inputs,
                    levels[level].0[next],
                    &[0, op, w, h, has_rhs, parity],
                );
                current[level] = next;
            }
        }
    };
    sweeps(builder, current);
    if level + 1 < sizes.len() {
        let coarse = level + 1;
        let (cw, ch) = sizes[coarse];
        let next = 1 - current[coarse];
        builder.pass(
            "lf_repair_heal_clear",
            vec![levels[coarse].0[current[coarse]]],
            levels[coarse].0[next],
            &[0, op, cw, ch],
        );
        current[coarse] = next;
        let mut inputs = vec![
            levels[level].0[current[level]],
            levels[coarse].0[current[coarse]],
        ];
        if level > 0 {
            inputs.push(levels[level].1);
        }
        builder.pass(
            "lf_repair_heal_restrict",
            inputs,
            levels[coarse].1,
            &[0, op, w, h, has_rhs, 0, cw, ch],
        );
        v_cycle(builder, levels, sizes, op, current, coarse);
        let next = 1 - current[level];
        builder.pass(
            "lf_repair_heal_prolong",
            vec![
                levels[level].0[current[level]],
                levels[coarse].0[current[coarse]],
            ],
            levels[level].0[next],
            &[0, op, w, h, cw, ch],
        );
        current[level] = next;
    }
    sweeps(builder, current);
}
