//! The window walk a GPU plan reads: the rectangle of each stage that a requested rectangle of the
//! output stage reads, walked back from the output through every boundary, each grown by what that
//! boundary needs around what it reads.
//!
//! [`WindowPlan::of_gpu_rect`] is that walk, in `O(segments)` with no pixel read. It is planned for
//! the GPU, which evaluates every boundary of a plan over the window it holds:
//!
//! - **A resample** reads its taps with their margin ([`crate::modules::Resample::reads`]),
//!   clamped to the stage edge where a tap is clamped to it.
//! - **A spatial operation** reads its output's rectangle grown by the operation's summed halo and
//!   clamped to the stage ([`super::SpatialEntry::halo_reads`]). A GPU spatial step evaluates every
//!   pixel of the window it holds at once, so it needs no tile grid, and a global estimate is the
//!   whole stage's, computed by the plan's light link, so no estimate bears on a window.
//!
//! A segment whose output is cut and that writes point replacements cannot be cut, and a rectangle
//! that maps outside a stage cannot be planned; both are named ([`RegionFallback`]).

use super::{Compiled, Segment};
use crate::modules::{Region, Stage};

/// Why a requested output rectangle cannot be cut out of a stack, which a GPU plan reports as
/// unplannable with this reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionFallback {
    Empty,
    PointReplacement,
    UnplannableGeometry,
}

impl RegionFallback {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Empty => "the requested viewport lies outside the output stage",
            Self::PointReplacement => "a point replacement cannot yet be cut to a viewport",
            Self::UnplannableGeometry => "the viewport cannot be mapped safely through the stack",
        }
    }
}

/// What a requested rectangle of a stack's output stage reads of each stage before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowPlan {
    /// Per segment, the rectangle of the whole stage it receives that its part of the requested
    /// rectangle reads through its exact geometry.
    reads: Vec<Region>,
}

/// The stage segment `index` reads: the source for the first, or the whole stage the boundary
/// entering it produces ([`super::Entry::stage`]).
fn input_stage(segments: &[Segment], index: usize, source: Stage) -> Stage {
    match &segments[index].entry {
        None => source,
        Some(entry) => entry.stage(segments[index - 1].stage()),
    }
}

impl WindowPlan {
    /// The windows a non-empty `requested` rectangle of `compiled`'s output stage reads, over a
    /// source of `source` dimensions, each boundary planned as the GPU evaluates it
    /// ([`super::Entry::plan_gpu_window`]). `O(segments)`, and reads no pixel.
    pub(crate) fn of_gpu_rect(
        compiled: &Compiled,
        source: (u32, u32),
        requested: Region,
    ) -> Result<Self, RegionFallback> {
        let segments = &compiled.segments;
        let source = Stage {
            width: source.0,
            height: source.1,
        };
        let count = segments.len();
        if count == 0 || requested.is_empty() {
            return Err(RegionFallback::Empty);
        }
        let output = segments[count - 1].stage();
        if requested.x1() > output.width || requested.y1() > output.height {
            return Err(RegionFallback::UnplannableGeometry);
        }
        let mut reads = vec![Region::whole(output); count];
        // What the segment being walked must produce, in its output stage's coordinates.
        let mut needed = requested;
        for index in (0..count).rev() {
            let segment = &segments[index];
            if needed.is_empty() {
                return Err(RegionFallback::UnplannableGeometry);
            }
            if needed != Region::whole(segment.stage()) && segment.has_pixels {
                return Err(RegionFallback::PointReplacement);
            }
            let input = input_stage(segments, index, source);
            let read = segment.geometry.unmap_region(needed);
            if read.x1() > input.width || read.y1() > input.height {
                return Err(RegionFallback::UnplannableGeometry);
            }
            reads[index] = read;
            if let Some(entry) = &segment.entry {
                needed = entry.plan_gpu_window(read, segments[index - 1].stage())?;
            }
        }
        Ok(Self { reads })
    }

    /// What a non-empty `requested` rectangle of the stage segment `end` of `compiled` produces
    /// reads of the stage segment `from`'s entry reads, `from` at most `end`, over a source of
    /// `source` dimensions: the source's window for segment 0, and for a later one the rectangle of
    /// the stage before its entry, grown by what the entry needs around what the segment reads.
    /// The walk [`Self::of_gpu_rect`] makes, over the segments from `end` back to `from` alone: what
    /// a staged sweep of those segments reads of the stage the sweep before it wrote.
    /// `O(segments)`, and reads no pixel.
    pub(crate) fn gpu_window_between(
        compiled: &Compiled,
        source: (u32, u32),
        from: usize,
        end: usize,
        requested: Region,
    ) -> Result<Region, RegionFallback> {
        let segments = &compiled.segments;
        let source = Stage {
            width: source.0,
            height: source.1,
        };
        if end >= segments.len() || from > end || requested.is_empty() {
            return Err(RegionFallback::Empty);
        }
        let produced = segments[end].stage();
        if requested.x1() > produced.width || requested.y1() > produced.height {
            return Err(RegionFallback::UnplannableGeometry);
        }
        let mut needed = requested;
        for index in (from..=end).rev() {
            let segment = &segments[index];
            if needed.is_empty() {
                return Err(RegionFallback::UnplannableGeometry);
            }
            if needed != Region::whole(segment.stage()) && segment.has_pixels {
                return Err(RegionFallback::PointReplacement);
            }
            let input = input_stage(segments, index, source);
            let read = segment.geometry.unmap_region(needed);
            if read.x1() > input.width || read.y1() > input.height {
                return Err(RegionFallback::UnplannableGeometry);
            }
            needed = match &segment.entry {
                Some(entry) => entry.plan_gpu_window(read, segments[index - 1].stage())?,
                None => read,
            };
        }
        Ok(needed)
    }

    /// The rectangle of the whole stage segment `segment` receives that its part of the requested
    /// rectangle reads: the source's window for the first segment, and for a later one the part of
    /// its boundary's output a GPU preview's boundary in it holds.
    pub(crate) fn reads(&self, segment: usize) -> Region {
        self.reads[segment]
    }

    /// [`Self::reads`], when it is less than the whole stage segment `segment` of `compiled`
    /// receives from a source of `source` dimensions; `None` when the segment reads all of it.
    pub(crate) fn received_cut(
        &self,
        compiled: &Compiled,
        source: (u32, u32),
        segment: usize,
    ) -> Option<Region> {
        let whole = input_stage(
            compiled.segments.get(..=segment)?,
            segment,
            Stage {
                width: source.0,
                height: source.1,
            },
        );
        let received = self.reads(segment);
        (received != Region::whole(whole)).then_some(received)
    }
}
