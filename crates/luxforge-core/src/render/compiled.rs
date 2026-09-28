//! The compiled IR: a recipe as the ordered rasterizing segments the registry compiles it into,
//! with the stage boundary that produces each one's input frame.

use super::window;
use crate::{
    mask_field::MaskField,
    modules::{ExactGeometry, Processing, Region, Resample, SpatialOperation, Stage},
};

/// What produces one segment's input frame, and therefore what separates it from the segment
/// before it. Both kinds are stage boundaries: the frame before them is finished, they read it and
/// write the next one.
#[derive(Clone)]
pub(crate) enum Entry {
    /// An interpolating boundary that also changes the stage.
    Resample(Resample),
    /// A neighbourhood boundary at the same dimensions as the stage it receives. `prefix_hash` is
    /// the SHA-256 of the canonical JSON of the layers before this one, which together with the
    /// source and the stage identifies what a global estimate was prepared from.
    Spatial {
        operation: SpatialOperation,
        prefix_hash: String,
        /// The global estimates this operation is handed instead of reducing its own stage: set
        /// only by a windowed proxy, whose stage is a window that cannot be reduced as a whole
        /// ([`window`]). `None` everywhere else.
        globals: Option<window::Globals>,
    },
}

impl Entry {
    /// The resample this entry is, if it is one. Point queries and the linear path walk a resample
    /// by its inverse mapping; a spatial entry maps its input pixel to itself.
    pub(crate) fn resample(&self) -> Option<Resample> {
        match self {
            Self::Resample(resample) => Some(*resample),
            Self::Spatial { .. } => None,
        }
    }
}

/// One rasterizing pass: the exact operations that share an input frame, their composed geometry and
/// the stage they produce. `entry` is what produces this segment's input frame, so consecutive
/// segments are separated by exactly one resample or one spatial operation, and the first segment
/// reads the source.
#[derive(Clone)]
pub(crate) struct Segment {
    pub(crate) entry: Option<Entry>,
    pub(crate) operations: Vec<Processing>,
    pub(crate) geometry: ExactGeometry,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) has_pixels: bool,
    pub(crate) has_color: bool,
    /// Where the frame this segment's resample entry reads lies in the stage the resample was
    /// compiled against: `(0, 0)`, except behind a windowed proxy's cut ([`window`]).
    pub(crate) entry_origin: (u32, u32),
    /// The rectangle of the resample's full output stage held by a cut entry frame. Its integer
    /// origin is added before the resample's floating-point inverse map, preserving exact taps.
    pub(crate) entry_window: Option<Region>,
    /// The segment output's first pixel in its original uncut output stage. Pointwise finish
    /// units read these original coordinates even when a viewport keeps only a rectangle.
    pub(crate) output_origin: (u32, u32),
}

impl Segment {
    /// The stage this segment produces.
    pub(crate) fn stage(&self) -> Stage {
        Stage {
            width: self.width,
            height: self.height,
        }
    }

    pub(crate) fn new(entry: Option<Entry>, width: u32, height: u32) -> Self {
        Self {
            entry,
            operations: Vec::new(),
            geometry: ExactGeometry::identity(width, height),
            width,
            height,
            has_pixels: false,
            has_color: false,
            entry_origin: (0, 0),
            entry_window: None,
            output_origin: (0, 0),
        }
    }

    /// The rectangle of its resample entry's full output stage this segment's input frame holds:
    /// [`Self::entry_window`], or the whole stage.
    pub(crate) fn resample_window(&self, resample: Resample) -> Region {
        self.entry_window.unwrap_or(Region::whole(Stage {
            width: resample.output_width,
            height: resample.output_height,
        }))
    }

    #[inline]
    pub(crate) fn resample_output_at(&self, x: u32, y: u32) -> (u32, u32) {
        match self.entry_window {
            Some(window) => (window.x0 + x, window.y0 + y),
            None => (x, y),
        }
    }

    /// Whether this pass writes anything into its frame. An identity pass that does not shares the
    /// source allocation instead of copying it.
    pub(super) fn writes_pixels(&self) -> bool {
        self.has_pixels || self.has_color
    }
}

/// One recipe compiled by the registry: the ordered rasterizing passes and the resamples between
/// them. A recipe without a resample is one segment, which is the M1 and M2 behavior unchanged.
///
/// `Clone` holds no pixels, only the operation lists and geometry `O(layers)` compiling already
/// allocated, so cloning a compiled prefix out of a cache to reuse it for several sampled points is
/// far cheaper than recompiling it.
#[derive(Clone)]
pub(crate) struct Compiled {
    pub(crate) segments: Vec<Segment>,
}

impl Compiled {
    pub(super) fn last(&self) -> &Segment {
        self.segments
            .last()
            .expect("a compiled recipe always has one segment")
    }

    pub(crate) fn stage(&self) -> Stage {
        self.last().stage()
    }

    /// Whether any mask in this compilation had the thin-feature rule applied to it: it draws a
    /// feature narrower than two pixels of the stage it was compiled against, so it is evaluated
    /// with a 2 x 2 supersample per pixel and the frame it produces is approximate.
    ///
    /// Read from the compilation the render itself uses rather than recomputed from the recipe, so
    /// what is reported and what is drawn cannot disagree. Cost is `O(layers)` and reads no pixels.
    pub(crate) fn supersampled_masks(&self) -> bool {
        self.segments.iter().any(|segment| {
            segment.operations.iter().any(|operation| match operation {
                Processing::Color(colour) => colour.mask().is_some_and(MaskField::supersampled),
                Processing::Spatial(spatial) => spatial.mask().is_some_and(MaskField::supersampled),
                _ => false,
            })
        })
    }

    /// Why a frame of this compilation is an approximation of the exact render at its size: a
    /// spatial operation, whose neighbourhoods scale with the stage, and a mask the proxy phase
    /// supersampled. `O(layers + components)`, no pixel read.
    pub(crate) fn approximation(&self) -> crate::ProxyApproximation {
        crate::ProxyApproximation {
            spatial: self.evaluates_spatial(),
            mask: self.supersampled_masks(),
            reduced_detail: false,
        }
    }

    /// Whether answering one pixel of this compilation evaluates a spatial segment.
    ///
    /// A spatial point query is the declared exception to [performance rule
    /// 4](../../docs/engineering/performance-rules.md#rules): it evaluates the stage-aligned tiles
    /// its pixels need, each once per query, so a caller that asks per display cell over the whole
    /// stage evaluates every tile of it. The coverage overlay reads this to refuse rather than to
    /// pay it. `O(segments)` and reads no pixels.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.spatial_before(self.segments.len())
    }

    /// Whether a spatial segment comes before segment `index`, so that, in a point query, the stage
    /// `index` reads comes through [`super::spatial::PointTiles`] rather than from the source alone.
    pub(crate) fn spatial_before(&self, index: usize) -> bool {
        self.segments[..index]
            .iter()
            .any(|segment| matches!(segment.entry, Some(Entry::Spatial { .. })))
    }
}

/// Where one segment-output pixel comes from: the input-frame pixel it reads and the replacement
/// that wins there, with that replacement's position in the operation list. The position decides
/// which colour runs still reach the pixel: a replacement overwrites everything the runs before it
/// produced, and only the runs after it process the replaced value.
pub(super) struct Resolved {
    pub(super) replacement: Option<(usize, [u8; 3])>,
    pub(super) input_x: u32,
    pub(super) input_y: u32,
}

impl Segment {
    pub(super) fn resolve(&self, x: u32, y: u32) -> Option<Resolved> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let (input_x, input_y) = self.geometry.unmap(x, y);
        let mut suffix = ExactGeometry::identity(self.width, self.height);
        for (index, operation) in self.operations.iter().enumerate().rev() {
            match operation {
                Processing::ExactGeometry(step) => suffix = step.then(suffix),
                Processing::PointReplace {
                    x: pixel_x,
                    y: pixel_y,
                    rgb,
                } if suffix.map(*pixel_x, *pixel_y) == Some((x, y)) => {
                    return Some(Resolved {
                        replacement: Some((index, *rgb)),
                        input_x,
                        input_y,
                    });
                }
                // A point replacement mapping outside this stage was cropped away.
                Processing::PointReplace { .. } => {}
                // Colour is applied to the resolved value, not to the coordinate walk, and a
                // resample or spatial operation is the next segment's entry, never one of its
                // operations.
                Processing::Color(_) | Processing::Spatial(_) | Processing::Resample(_) => {}
            }
        }
        Some(Resolved {
            replacement: None,
            input_x,
            input_y,
        })
    }
}

/// Where each of one segment's point replacements lands in its output frame, in stack order, with
/// its position in the operation list. One backward walk accumulates the suffix geometry that
/// carries each replacement; a replacement a later crop discards is simply absent. The result is
/// bounded by the layer count and reads no pixels.
pub(super) fn mapped_replacements(segment: &Segment) -> Vec<(usize, u32, u32, [u8; 3])> {
    let mut suffix = ExactGeometry::identity(segment.width, segment.height);
    let mut mapped = Vec::new();
    for (index, operation) in segment.operations.iter().enumerate().rev() {
        match operation {
            Processing::ExactGeometry(step) => suffix = step.then(suffix),
            Processing::PointReplace {
                x: pixel_x,
                y: pixel_y,
                rgb,
            } => {
                if let Some((x, y)) = suffix.map(*pixel_x, *pixel_y) {
                    mapped.push((index, x, y, *rgb));
                }
            }
            Processing::Color(_) | Processing::Spatial(_) | Processing::Resample(_) => {}
        }
    }
    mapped.reverse();
    mapped
}
