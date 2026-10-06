//! Staged sweeps (`docs/design/gpu-first.md`, "Staged sweeps"): a stack whose spatial layers
//! together reach far is drawn in sweeps, each sweep a run of its links over tiles of the stage it
//! writes, so a tile's window carries only its own sweep's halos and leads rather than every
//! layer's. Planned here, with no pixel read and nothing that names a GPU crate; the photo surface
//! and the tile runner draw them.
//!
//! - **Grouping.** The plan's spatial operations, each with the colour operations after it, are
//!   grouped in order. A sweep ends before a spatial operation only where the halos and leads of
//!   the operations already in it pass [`SWEEP_SPLIT_REACH`]: Detail rides with the Presence layer
//!   after it, and a stack whose one large reach is its last link is drawn chained, as before. The
//!   first sweep also runs the content operations before the first spatial one, the last the
//!   geometry tail and the output operations after the last.
//! - **Staged lights.** An operation reading a light whose input is the spatial operations before
//!   it ([`super::GpuLightInput::Stage`], Dehaze behind Clarity or Detail) always starts a sweep,
//!   whatever the reach, so the stage texture that sweep reads holds the light's exact input; the
//!   sweep before it covers the whole content stage, and the light is reduced from the texture
//!   before the reading sweep's first tile ([`GpuSweep::lights`]).
//! - **Stage textures.** Every sweep but the last writes its tiles' rectangles of its last link's
//!   output, the content stage in the boundary's format, into a stage texture the size of the
//!   content stage; the next sweep cuts its windows from it. At most [`SWEEP_STAGE_TEXTURES`] are
//!   held, used in turn, each charged before it is created.
//! - **Windows.** A sweep's tile reads the window its own links need of the stage before them
//!   ([`WindowPlan::gpu_window_between`]), anchored to its own links ([`anchor_of`]), so every
//!   texel a sweep writes is the whole stage's, bit for bit, whatever tile wrote it, and the last
//!   sweep's codes are the chained tiles' own. A sweep before the last covers the bounding box of
//!   every window the sweep after it reads.
//! - **Sides.** Each sweep takes the longest side whose middle tile's slot ([`super::preview`]'s
//!   `links_bytes`) and light links fit the budget beside the stage textures and whose window
//!   carries at most [`super::REST_TILE_WORK`]. A stack one of whose sweeps fits no side is drawn
//!   chained, and the figures say why ([`Chained`]).
use super::{
    GpuAnchor, GpuPlan, REST_TILE_WORK, RestTile, anchored,
    plan::anchor_of,
    preview::{Links, lights_bytes, links_bytes},
};
use crate::{
    BoundaryFormat,
    modules::{Region, Stage},
    render::{Compiled, compiled::Entry, window::WindowPlan},
};
use std::ops::Range;

/// The halos and leads, in pixels of the content stage, that the spatial operations already in a
/// sweep may carry before the next spatial operation starts a sweep of its own.
pub const SWEEP_SPLIT_REACH: u32 = 64;

/// The most stage textures a staged stack holds at once, used in turn: a sweep reads the one the
/// sweep before it wrote and writes the other.
pub const SWEEP_STAGE_TEXTURES: u32 = 2;

/// How a stack's tiles are drawn: in staged sweeps, or chained, every tile running every link,
/// and why.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuStaging {
    Staged(Box<GpuSweeps>),
    Chained(Chained),
}

impl GpuStaging {
    /// The sweeps, when the stack is staged.
    pub fn sweeps(&self) -> Option<&GpuSweeps> {
        match self {
            Self::Staged(sweeps) => Some(sweeps),
            Self::Chained(_) => None,
        }
    }
}

/// Why a stack is drawn chained rather than in staged sweeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chained {
    /// Its links make one sweep: no spatial operation's reach before another passes
    /// [`SWEEP_SPLIT_REACH`].
    OneSweep,
    /// A sweep fits no side: what its smallest tile's slot, light links and the stage textures
    /// take together, and the budget.
    OverBudget { needed: u64, budget: u64 },
    /// The compilation's spatial boundaries are not the plan's, or a sweep's window cannot be cut.
    Unplannable(String),
}

/// A stack's staged sweeps over its content stage ([`GpuStaging::Staged`]).
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSweeps {
    /// The sweeps in the order they are drawn, at least two: every one but the last writes a stage
    /// texture, and the last the stack's output, its tiles the picture's or the stream's own.
    pub sweeps: Vec<GpuSweep>,
    /// The content stage every stage texture holds: the plan's boundary stage.
    pub stage: Stage,
    /// How a stage texture's texels are held: as the boundary's and a chain's intermediates are,
    /// half floats on the byte path and `f32` on the linear path.
    pub format: BoundaryFormat,
    /// How many stage textures are held: one for two sweeps, [`SWEEP_STAGE_TEXTURES`] for more.
    pub textures: u32,
    /// What one stage texture takes: the content stage's texels in `format`.
    pub texture_bytes: u64,
}

impl GpuSweeps {
    /// What the stage textures take together.
    pub fn stage_bytes(&self) -> u64 {
        u64::from(self.textures) * self.texture_bytes
    }

    /// The largest a sweep's slot and its light links take beside the stage textures.
    pub fn slot_bytes(&self) -> u64 {
        self.sweeps
            .iter()
            .map(|sweep| sweep.slot_bytes)
            .max()
            .unwrap_or(0)
    }
}

/// One sweep of a staged stack.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSweep {
    /// The plan's spatial operations it runs, `plan.spatial[spatial]`, each with the colour
    /// operations after it.
    pub spatial: Range<usize>,
    /// It runs the content operations before the first spatial operation, and reads the source.
    pub first: bool,
    /// It runs the geometry tail and the output operations, and writes the stack's output.
    pub last: bool,
    /// Where its windows start: its own spatial operations' anchor.
    pub anchor: GpuAnchor,
    /// The halos and leads its spatial operations carry, in pixels of the content stage.
    pub reach: u32,
    /// The stage texture it cuts its windows from; `None` for the first, which cuts them from the
    /// source.
    pub reads: Option<u32>,
    /// The stage texture it copies its tiles' rectangles of its last link's output into; `None`
    /// for the last, whose tiles are the output's.
    pub writes: Option<u32>,
    /// The rectangle its tiles cover: of the content stage for every sweep but the last, the
    /// bounding box of what the sweep after it reads; of the output stage for the last.
    pub covers: Region,
    /// Its tiles' side.
    pub side: u32,
    /// Its tiles, each its rectangle of the stage it writes and the window it reads of the stage
    /// before it, anchored.
    pub tiles: Vec<RestTile>,
    /// What its middle tile's slot and its light links take by the plan's own figures.
    pub slot_bytes: u64,
    /// The plan's lights, `k` of [`GpuPlan::lights`], whose input is the stage texture it reads
    /// ([`super::GpuLightInput::Stage`]): each reduced from the whole texture before its first
    /// tile, which its first operation reads. Empty for a sweep whose lights, if any, are over the
    /// source.
    pub lights: Vec<usize>,
}

/// What one spatial operation carries into a window: its summed halo and the larger of its
/// anchor's leads, in pixels of its stage.
fn reach(spatial: &super::GpuSpatial) -> u32 {
    let lead = anchor_of(std::slice::from_ref(spatial)).lead;
    spatial.halos.iter().sum::<u32>() + lead.0.max(lead.1)
}

/// The staged lights operation `spatial` of `plan` reads ([`super::GpuLightInput::Stage`]): their
/// places among the plan's lights.
fn staged_lights<'a>(
    plan: &'a GpuPlan,
    spatial: &'a [super::GpuSpatial],
) -> impl Iterator<Item = usize> + 'a {
    spatial.iter().filter_map(|operation| {
        operation.light?;
        plan.light_of(operation.layer)
            .filter(|k| plan.lights[*k].staged())
    })
}

/// `plan`'s spatial operations grouped into sweeps, in order: a sweep ends before an operation
/// where the reach of those already in it passes [`SWEEP_SPLIT_REACH`], and before every operation
/// reading a staged light. One range, `0..0`, for a plan with none. `O(passes)`.
pub(crate) fn sweep_ranges(plan: &GpuPlan) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let (mut start, mut carried) = (0, 0);
    for (index, spatial) in plan.spatial.iter().enumerate() {
        let staged = staged_lights(plan, std::slice::from_ref(spatial))
            .next()
            .is_some();
        if index > start && (carried > SWEEP_SPLIT_REACH || staged) {
            ranges.push(start..index);
            (start, carried) = (index, 0);
        }
        carried += reach(spatial);
    }
    ranges.push(start..plan.spatial.len());
    ranges
}

/// The order a sweep's tiles are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TileOrder {
    /// By slot shape, row by row within a shape: a picture at rest's.
    ByShape,
    /// Row by row from the covered rectangle's origin: a stream's, whose bands are its rows.
    Rows,
}

/// What a staged plan is planned with.
pub(crate) struct SweepRequest<'a> {
    pub(crate) compiled: &'a Compiled,
    /// The source's dimensions, which the first sweep's windows are rectangles of.
    pub(crate) source: (u32, u32),
    pub(crate) plan: &'a GpuPlan,
    pub(crate) format: BoundaryFormat,
    /// The sides each sweep is tried at, longest first.
    pub(crate) sides: &'a [u32],
    /// What a sweep's slot, its light links and the stage textures may take together; `None` takes
    /// the first side whatever it takes, for a caller that names its side.
    pub(crate) budget: Option<u64>,
    pub(crate) order: TileOrder,
}

/// `request`'s plan in staged sweeps ([`GpuSweeps`]), or why it is drawn chained. Planned from the
/// last sweep back, each earlier sweep covering what the one after it reads: `O(sweeps × sides ×
/// segments + tiles × segments)`, no pixel read.
pub(crate) fn plan_sweeps(request: &SweepRequest) -> GpuStaging {
    let SweepRequest {
        compiled,
        source,
        plan,
        format,
        sides,
        budget,
        order,
    } = *request;
    let ranges = sweep_ranges(plan);
    if ranges.len() < 2 {
        return GpuStaging::Chained(Chained::OneSweep);
    }
    // The segment each spatial operation enters: its compiled boundary.
    let entries: Vec<usize> = compiled
        .segments
        .iter()
        .enumerate()
        .filter(|(_, segment)| matches!(segment.entry, Some(Entry::Spatial(_))))
        .map(|(index, _)| index)
        .collect();
    if entries.len() != plan.spatial.len() {
        return GpuStaging::Chained(Chained::Unplannable(format!(
            "{} spatial boundaries compiled for {} planned spatial operations",
            entries.len(),
            plan.spatial.len()
        )));
    }
    let stage = plan.boundary.stage;
    let texture_bytes =
        u64::from(stage.width) * u64::from(stage.height) * format.texel_bytes() as u64;
    let textures = (ranges.len() as u32 - 1).min(SWEEP_STAGE_TEXTURES);
    let stage_bytes = u64::from(textures) * texture_bytes;
    let last_segment = compiled.segments.len() - 1;
    let mut sweeps: Vec<GpuSweep> = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.iter().enumerate().rev() {
        let (first, last) = (index == 0, index == ranges.len() - 1);
        // The segment the sweep's windows are cut before, and the one whose output it writes.
        let from = if first { 0 } else { entries[range.start] };
        let end = if last {
            last_segment
        } else {
            entries[ranges[index + 1].start] - 1
        };
        let produced = compiled.segments[end].stage();
        if !last && produced != stage {
            return GpuStaging::Chained(Chained::Unplannable(format!(
                "a sweep writes a {}x{} stage, not the {}x{} content stage",
                produced.width, produced.height, stage.width, stage.height
            )));
        }
        // A sweep before one reading a staged light writes the whole stage the light reduces.
        let covers = match sweeps.last() {
            None => Region::whole(produced),
            Some(after) if !after.lights.is_empty() => Region::whole(produced),
            Some(after) => bounding(after.tiles.iter().map(|tile| tile.window)),
        };
        let anchor = anchor_of(&plan.spatial[range.clone()]);
        let window_of = |rect: Region| {
            WindowPlan::gpu_window_between(compiled, source, from, end, rect)
                .map(|window| anchored(window, anchor))
                .map_err(|reason| {
                    Chained::Unplannable(format!(
                        "sweep {index}'s tile at ({}, {}): {}",
                        rect.x0,
                        rect.y0,
                        reason.reason()
                    ))
                })
        };
        let links = Links {
            spatial: range.clone(),
            first,
            last,
        };
        let work = range.len().max(1) as u64;
        // The longest side whose middle tile fits; the smallest's figures name a stack none fits.
        let mut chosen = None;
        let mut smallest = 0;
        for &side in sides {
            let (width, height) = (side.min(covers.width), side.min(covers.height));
            let middle = Region {
                x0: covers.x0 + (covers.width - width) / 2,
                y0: covers.y0 + (covers.height - height) / 2,
                width,
                height,
            };
            let window = match window_of(middle) {
                Ok(window) => window,
                Err(chained) => return GpuStaging::Chained(chained),
            };
            let slot = links_bytes(plan, &links, window, format, (width, height), true)
                + lights_bytes(plan, range.clone(), format);
            smallest = slot;
            let fits = budget.is_none_or(|budget| {
                slot + stage_bytes <= budget && window.pixels() * work <= REST_TILE_WORK
            });
            if fits {
                chosen = Some((side, slot));
                break;
            }
        }
        let Some((side, slot_bytes)) = chosen else {
            return GpuStaging::Chained(Chained::OverBudget {
                needed: smallest + stage_bytes,
                budget: budget.unwrap_or(0),
            });
        };
        let mut tiles = Vec::new();
        for y0 in (covers.y0..covers.y1()).step_by(side as usize) {
            for x0 in (covers.x0..covers.x1()).step_by(side as usize) {
                let rect = Region {
                    x0,
                    y0,
                    width: side.min(covers.x1() - x0),
                    height: side.min(covers.y1() - y0),
                };
                match window_of(rect) {
                    Ok(window) => tiles.push(RestTile { rect, window }),
                    Err(chained) => return GpuStaging::Chained(chained),
                }
            }
        }
        if order == TileOrder::ByShape {
            tiles = super::preview::by_shape(tiles);
        }
        sweeps.push(GpuSweep {
            spatial: range.clone(),
            first,
            last,
            anchor,
            reach: plan.spatial[range.clone()].iter().map(reach).sum(),
            reads: (!first).then(|| (index as u32 - 1) % textures),
            writes: (!last).then(|| index as u32 % textures),
            covers,
            side,
            tiles,
            slot_bytes,
            lights: match first {
                true => Vec::new(),
                false => staged_lights(plan, &plan.spatial[range.clone()]).collect(),
            },
        });
    }
    sweeps.reverse();
    GpuStaging::Staged(Box::new(GpuSweeps {
        sweeps,
        stage,
        format,
        textures,
        texture_bytes,
    }))
}

/// The smallest rectangle holding every one of `regions`, or an empty one for none.
fn bounding(regions: impl Iterator<Item = Region>) -> Region {
    let mut held: Option<(u32, u32, u32, u32)> = None;
    for region in regions.filter(|region| !region.is_empty()) {
        held = Some(match held {
            None => (region.x0, region.y0, region.x1(), region.y1()),
            Some((x0, y0, x1, y1)) => (
                x0.min(region.x0),
                y0.min(region.y0),
                x1.max(region.x1()),
                y1.max(region.y1()),
            ),
        });
    }
    held.map_or(Region::EMPTY, |(x0, y0, x1, y1)| Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}
