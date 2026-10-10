//! Clone and Heal: the repair layer's CPU reference units (`docs/design/corrections-numerics.md`).
//!
//! Pure functions over a bounded planar `f32` linear-RGB frame, the representation the repair stage
//! receives after source development and before Detail and colour: a JPEG's decoded codes, a RAW's
//! continuous development (values outside `[0, 1]` kept). Nothing here is wired into the render
//! pipeline yet; the render-integration task compiles a repair layer into [`RepairOp`]s and runs
//! [`apply`] over a frame holding every pixel the operations declare they read
//! ([`RepairOp::reads`]).
//!
//! Each unit is the production transcription of `luxforge_reference::repair`, the independent
//! `f64` reference, and is held to it bit for bit by the exact-buffer tests in `tests.rs`:
//!
//! - [`footprint`]: coverage, the mask brush's capsule profile times `opacity / 100`.
//! - [`RepairOp::new`]: the source offset rounded to a whole stage pixel, half away from zero.
//! - [`clone`]: `(1 - a) * I(p) + a * I(p + delta)`, in place, rows ordered away from the source
//!   so no pixel reads the operation's own output.
//! - [`heal`]: the membrane correction in the sRGB-encoded domain, solved by the frozen multigrid
//!   schedule over tile-sparse levels.
//! - [`search`]: the deterministic automatic source.
//! - [`apply`]: operations in stored order, each reading only what the ones before it left, its
//!   output rounded to `f32` as the frame holds it.
//!
//! A destination pixel whose source lies outside the stage is not repaired. Every operation is
//! bounded before it allocates: Heal's scratch is reserved up front and an operation past
//! [`HEAL_CELLS_MAX`] is refused with `resource-limit`, naming it; nothing changes its geometry or
//! quality to fit. Every pass checks its cancellation token per row or per sweep.

pub(crate) mod footprint;
pub(crate) mod gpu;
pub(crate) mod heal;
pub(crate) mod patch;
#[cfg(feature = "qualification")]
pub mod qualification;
#[allow(
    dead_code,
    reason = "the Corrections module's automatic source (TASK-005) calls the search"
)]
pub(crate) mod search;

#[cfg(test)]
mod tests;

use crate::{
    Cancel, Error, GpuProgram,
    modules::{Region, Stage},
    path,
    render::gpu::GpuProgramKind,
};
use footprint::Footprint;

/// The repair program (`repair.wgsl`): one group of operations' Clone and Heal under the spatial
/// convention, with each operation's tiles in a per-level atlas ([`gpu`]). Not enabled: no plan
/// names it until the render integration does, and its kernels are held to these units by the
/// desktop's `gpu_repair` readback tests.
pub(crate) static REPAIR_PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_repair",
    source: include_str!("repair.wgsl"),
    kind: GpuProgramKind::Spatial,
    words: 0,
    enabled: false,
};

/// The kernels and the apply of [`REPAIR_PROGRAM`].
#[cfg(test)]
pub(crate) fn gpu_functions() -> (Vec<&'static str>, Vec<&'static str>) {
    (
        vec![
            "lf_repair_gather",
            "lf_repair_coarse",
            "lf_repair_start",
            "lf_repair_relax",
            "lf_repair_restrict",
            "lf_repair_correct",
            "lf_repair_composite",
            "lf_repair_blend",
        ],
        vec!["lf_repair_apply", "lf_repair_apply_over"],
    )
}

/// Which repair an operation performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepairKind {
    Clone,
    Heal,
    /// A frozen patch: immutable pixels composited at their rectangle ([`patch`]).
    Patch,
}

/// The most level-0 cells one Heal operation's solve may hold: its footprint and boundary ring,
/// counted in the 16 x 16 tiles the solver allocates. At about 41 bytes a cell over every level
/// (`f64` values, a class byte, a right-hand side on the coarse levels) the bound is about 350 MB
/// of scratch, held only while the operation solves. It admits a spot of radius `0.25` content
/// heights on a 60 MP photograph (about 8.0 million cells with tile rounding) and refuses larger.
pub(crate) const HEAL_CELLS_MAX: usize = 8 << 20;

/// One stored operation bound to a stage.
#[derive(Clone, Debug)]
pub(crate) struct RepairOp {
    pub(crate) kind: RepairKind,
    pub(crate) footprint: Footprint,
    /// Source minus destination in whole stage pixels.
    pub(crate) delta: [i64; 2],
    /// A frozen patch's pixels at the stage, for [`RepairKind::Patch`]; its footprint is empty.
    pub(crate) patch: Option<std::sync::Arc<patch::BoundPatch>>,
}

impl RepairOp {
    /// An operation of `kind` over the stored path `points` (normalized positions, `x` a fraction
    /// of the content width and `y` of its height), `radius` in content heights, `feather` and
    /// `opacity` in `0..=100`, and the stored offset in grid steps (source anchor minus destination
    /// anchor), bound to `stage`.
    ///
    /// The offset becomes `round(k / 16384 * W)` and `round(k / 16384 * H)` pixels, half away from
    /// zero: a grid step is a power-of-two fraction, so both products are exact and every stage
    /// size rounds the same value.
    pub(crate) fn new(
        kind: RepairKind,
        points: &[[f64; 2]],
        radius: f64,
        feather: f64,
        opacity: f64,
        offset: [i32; 2],
        stage: Stage,
    ) -> Self {
        let delta = delta(offset, stage);
        Self {
            kind,
            footprint: Footprint::new(points, radius, feather, opacity, stage),
            delta,
            patch: None,
        }
    }

    /// A frozen patch bound to its stage: it reads and writes its own region and reads no source.
    pub(crate) fn patch(bound: patch::BoundPatch) -> Self {
        let stage = bound.stage();
        Self {
            kind: RepairKind::Patch,
            footprint: Footprint::new(&[], 0.0, 0.0, 0.0, stage),
            delta: [0, 0],
            patch: Some(std::sync::Arc::new(bound)),
        }
    }

    pub(crate) fn stage(&self) -> Stage {
        self.footprint.stage()
    }

    /// Whether stage pixel `(x, y)`'s source lies inside the stage.
    #[inline]
    pub(crate) fn valid(&self, x: u32, y: u32) -> bool {
        let stage = self.stage();
        let sx = i64::from(x) + self.delta[0];
        let sy = i64::from(y) + self.delta[1];
        sx >= 0 && sy >= 0 && sx < i64::from(stage.width) && sy < i64::from(stage.height)
    }

    /// The repaired coverage at `(x, y)`: the footprint's coverage where the source is valid, else
    /// zero.
    #[inline]
    pub(crate) fn alpha(&self, x: u32, y: u32) -> f64 {
        if self.valid(x, y) {
            self.footprint.coverage(x, y)
        } else {
            0.0
        }
    }

    /// The pixels this operation may write: its support, or a patch's region.
    pub(crate) fn writes(&self) -> Region {
        match &self.patch {
            Some(patch) => patch.region(),
            None => self.footprint.support(),
        }
    }

    /// The pixels this operation reads, as two rectangles of the stage (either may be empty): its
    /// destination and its source, each grown by one pixel for Heal's boundary ring, clipped to
    /// the stage. A source farther than any halo is declared here, not by growing one rectangle.
    pub(crate) fn reads(&self) -> [Region; 2] {
        reads(self.kind, self.writes(), self.delta, self.stage())
    }
}

/// A stored offset in grid steps as whole pixels of `stage`: `round(k / 16384 * W)` across and
/// `round(k / 16384 * H)` down, half away from zero ([`RepairOp::new`]).
pub(crate) fn delta(offset: [i32; 2], stage: Stage) -> [i64; 2] {
    [
        (path::steps_to_units(offset[0]) * f64::from(stage.width)).round() as i64,
        (path::steps_to_units(offset[1]) * f64::from(stage.height)).round() as i64,
    ]
}

/// [`RepairOp::reads`] from an operation's kind, the pixels it may write, its offset in pixels and
/// its stage: what the host's region planning reads without building a footprint.
pub(crate) fn reads(kind: RepairKind, writes: Region, delta: [i64; 2], stage: Stage) -> [Region; 2] {
    // A patch blends over what it covers and reads nothing else.
    if kind == RepairKind::Patch {
        return [writes, Region::EMPTY];
    }
    let grow = u32::from(kind == RepairKind::Heal);
    let destination = writes.grown(grow, stage);
    let s = writes;
    if s.is_empty() {
        return [Region::EMPTY, Region::EMPTY];
    }
    let shift = |start: u32, delta: i64| i64::from(start) + delta;
    let (x0, y0) = (shift(s.x0, delta[0]), shift(s.y0, delta[1]));
    let (x1, y1) = (x0 + i64::from(s.width), y0 + i64::from(s.height));
    let g = i64::from(grow);
    let (x0, y0) = ((x0 - g).max(0), (y0 - g).max(0));
    let x1 = (x1 + g).min(i64::from(stage.width));
    let y1 = (y1 + g).min(i64::from(stage.height));
    let source = if x0 >= x1 || y0 >= y1 {
        Region::EMPTY
    } else {
        Region {
            x0: x0 as u32,
            y0: y0 as u32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        }
    };
    [destination, source]
}

/// Planar `f32` linear RGB the repair units read and write in place: one rectangle of the stage,
/// or several disjoint ones ([`Self::with_parts`]). Each part holds one complete R plane over its
/// rectangle, then G, then B, row by row. An operation's destination and its source each lie in
/// one part, so a pixel the operation reads is always held.
#[derive(Debug)]
pub(crate) struct RepairFrame<'a> {
    stage: Stage,
    parts: Vec<(Region, &'a mut [f32])>,
}

impl<'a> RepairFrame<'a> {
    #[cfg_attr(not(test), allow(dead_code, reason = "a frame of one rectangle, for the tests"))]
    pub(crate) fn new(stage: Stage, region: Region, values: &'a mut [f32]) -> Result<Self, Error> {
        Self::with_parts(stage, vec![(region, values)])
    }

    /// A frame of disjoint rectangles of `stage`, each with its three planes.
    pub(crate) fn with_parts(
        stage: Stage,
        parts: Vec<(Region, &'a mut [f32])>,
    ) -> Result<Self, Error> {
        for (region, values) in &parts {
            let fits = region.x1() <= stage.width && region.y1() <= stage.height;
            if !fits || values.len() as u64 != region.pixels().saturating_mul(3) {
                return Err(Error::internal(format!(
                    "a repair frame of {} values cannot hold {:?} of {}x{}",
                    values.len(),
                    region,
                    stage.width,
                    stage.height
                )));
            }
        }
        Ok(Self { stage, parts })
    }

    /// The part holding `(x, y)` and the pixel's index in it.
    #[inline]
    fn locate(&self, x: u32, y: u32) -> (usize, usize) {
        let part = match self.parts.len() {
            1 => 0,
            _ => self
                .parts
                .iter()
                .position(|(region, _)| region.contains(x, y))
                .expect("a repair operation reads only pixels its frame holds"),
        };
        let region = self.parts[part].0;
        (
            part,
            ((y - region.y0) as usize) * region.width as usize + (x - region.x0) as usize,
        )
    }

    /// Stage pixel `(x, y)`, which the frame holds.
    #[inline]
    pub(crate) fn get(&self, x: u32, y: u32) -> [f32; 3] {
        let (part, at) = self.locate(x, y);
        let (region, values) = &self.parts[part];
        let len = region.pixels() as usize;
        [values[at], values[len + at], values[2 * len + at]]
    }

    #[inline]
    pub(crate) fn set(&mut self, x: u32, y: u32, rgb: [f32; 3]) {
        let (part, at) = self.locate(x, y);
        let (region, values) = &mut self.parts[part];
        let len = region.pixels() as usize;
        values[at] = rgb[0];
        values[len + at] = rgb[1];
        values[2 * len + at] = rgb[2];
    }

    /// Whether every pixel `region` holds is in one part of the frame.
    fn holds(&self, region: Region) -> bool {
        region.is_empty()
            || self.parts.iter().any(|(held, _)| {
                region.x0 >= held.x0
                    && region.y0 >= held.y0
                    && region.x1() <= held.x1()
                    && region.y1() <= held.y1()
            })
    }
}

/// `(1 - a) * i + a * h` per channel in `f64`, rounded to `f32`: the one blend both repairs end
/// in.
#[inline]
fn blend(a: f64, i: [f32; 3], h: [f64; 3]) -> [f32; 3] {
    [0, 1, 2].map(|c| ((1.0 - a) * f64::from(i[c]) + a * h[c]) as f32)
}

/// Every operation of `ops`, in order, over `frame`: each reads only what the ones before it left
/// (its own output never), and writes its footprint's pixels rounded to `f32`. An operation whose
/// declared reads the frame does not hold is a host error, refused before anything is written by
/// it; a Heal past [`HEAL_CELLS_MAX`] is `resource-limit`, naming its index.
pub(crate) fn apply(
    frame: &mut RepairFrame<'_>,
    ops: &[RepairOp],
    cancel: &Cancel,
) -> Result<(), Error> {
    for (index, op) in ops.iter().enumerate() {
        if op.stage() != frame.stage {
            return Err(Error::internal(format!(
                "repair operation {index} was bound to another stage"
            )));
        }
        let [destination, source] = op.reads();
        if !frame.holds(destination) || !frame.holds(source) {
            return Err(Error::internal(format!(
                "repair operation {index} reads outside the frame it was given"
            )));
        }
        cancel.check()?;
        match op.kind {
            RepairKind::Clone => clone(frame, op, cancel)?,
            RepairKind::Patch => match &op.patch {
                Some(bound) => patch::composite(frame, bound, cancel)?,
                None => {
                    return Err(Error::internal(format!(
                        "repair operation {index} is a patch without its pixels"
                    )));
                }
            },
            RepairKind::Heal => heal::heal(frame, op, cancel).map_err(|error| {
                if error.kind == crate::ErrorKind::ResourceLimit {
                    Error::resource_limit(format!("repair operation {index}: {}", error.detail))
                } else {
                    error
                }
            })?,
        }
    }
    Ok(())
}

/// One Clone over `frame`, in place: every pixel `p` of the support with `a(p) > 0` becomes
/// `(1 - a) * I(p) + a * I(p + delta)`.
///
/// No scratch: rows are visited in the order that leaves every source row unwritten when it is
/// read — top to bottom when the source lies below, bottom to top when above — and, when the
/// source lies on the same row, the columns likewise. So `I` is always the frame before this
/// operation, as the reference reads it. Only the cells the footprint's index reaches are visited;
/// every other pixel's coverage is exactly zero.
pub(crate) fn clone(
    frame: &mut RepairFrame<'_>,
    op: &RepairOp,
    cancel: &Cancel,
) -> Result<(), Error> {
    let [dx, dy] = op.delta;
    let (rows, columns) = op.footprint.cells();
    let backwards_rows = dy < 0;
    let backwards_columns = dy == 0 && dx < 0;
    for band in ordered(0..rows, backwards_rows) {
        let cells: Vec<Region> = ordered(0..columns, backwards_columns)
            .filter_map(|column| op.footprint.cell_region(band, column))
            .collect();
        let Some(first) = cells.first() else {
            continue;
        };
        for y in ordered(first.y0..first.y1(), backwards_rows) {
            cancel.check()?;
            for cell in &cells {
                for x in ordered(cell.x0..cell.x1(), backwards_columns) {
                    let a = op.alpha(x, y);
                    if a > 0.0 {
                        let sx = (i64::from(x) + dx) as u32;
                        let sy = (i64::from(y) + dy) as u32;
                        let s = frame.get(sx, sy).map(f64::from);
                        let out = blend(a, frame.get(x, y), s);
                        frame.set(x, y, out);
                    }
                }
            }
        }
    }
    Ok(())
}

/// `range` forwards, or backwards when `backwards`.
fn ordered(range: std::ops::Range<u32>, backwards: bool) -> impl Iterator<Item = u32> {
    let (forwards, backwards) = if backwards {
        (None, Some(range.rev()))
    } else {
        (Some(range), None)
    };
    forwards
        .into_iter()
        .flatten()
        .chain(backwards.into_iter().flatten())
}

impl RepairOp {
    /// Every pixel the footprint's index reaches, cell by cell, row by row within a cell: the only
    /// pixels whose coverage may be above zero.
    pub(crate) fn reached(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let (rows, columns) = self.footprint.cells();
        (0..rows)
            .flat_map(move |row| (0..columns).map(move |column| (row, column)))
            .filter_map(|(row, column)| self.footprint.cell_region(row, column))
            .flat_map(|cell| {
                (cell.y0..cell.y1()).flat_map(move |y| (cell.x0..cell.x1()).map(move |x| (x, y)))
            })
    }
}
