//! A GPU preview's held input boundary (`docs/design/gpu-preview.md`, "The held input boundary"):
//! the input of the earliest layer a draft changes, rendered once per draft by the preview worker
//! and held by the photo surface while the draft is open.
//!
//! The boundary is the stage that layer receives — its segment's input through the exact steps
//! before it — with every colour operation before it in the same segment applied and nothing
//! quantized after them, so a boundary inside a colour run holds the unclamped `f32` value the run
//! hands the layer. It is written as `rgba16float` texels, the format the surface uploads, in one
//! pass over the rows it holds, on either pixel domain:
//!
//! - **The segment's input** is whatever the CPU render reads there: the (proxy) source for the
//!   first segment, or the frame a resample or a spatial operation writes, built exactly as a frame
//!   of the whole stack builds it ([`super::byte::frames`], [`super::Evaluation::frames_prefix`]), so
//!   an encoded byte boundary is the width the whole recipe chooses.
//! - **The pass** is the segment's own rows ([`super::segment_pass`]) over a stand-in segment that
//!   holds the operations before the layer, so its colour arithmetic and its masks are the CPU's.
//! - **A window.** A windowed proxy holds only the part of each stage its output reads, so the
//!   boundary holds the part of the received stage that window covers, with its origin there; the
//!   plan addresses the whole stage, and the surface offsets the texels by that origin.
//!
//! A half float holds every finite value up to 65,504. A CPU value past it is held at the largest
//! finite half of its sign, so the GPU never receives a value the CPU did not have as finite.
use super::{Compiled, Entry, Render, RenderSource, Segment, linear};
use crate::{
    Error,
    modules::{ExactGeometry, Processing, Region, Stage},
};
use std::{borrow::Cow, sync::Arc};

/// Bytes per boundary texel: four half floats.
pub(crate) const TEXEL_BYTES: usize = 8;

/// The most bytes one boundary may hold: 16 MP of texels. A Fit proxy is at most 8 MP, and a
/// windowed proxy's window is what the display shows plus the margins its boundaries need.
pub const BOUNDARY_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// The largest finite half float.
const HALF_MAX: f32 = 65504.0;

/// The byte length of a `width` × `height` boundary, or the limit it passes.
pub(super) fn frame_len(width: u32, height: u32) -> Result<usize, Error> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|texels| texels.checked_mul(TEXEL_BYTES as u64))
        .filter(|bytes| *bytes <= BOUNDARY_MAX_BYTES)
        .ok_or_else(|| {
            Error::resource_limit(format!(
                "a {width}x{height} GPU preview boundary exceeds {} MiB",
                BOUNDARY_MAX_BYTES / (1024 * 1024)
            ))
        })?;
    usize::try_from(bytes).map_err(|_| Error::resource_limit("boundary is not addressable"))
}

/// One texel written into the start of `bytes`: each channel as the nearest half float, a finite
/// value past the half range held at its largest finite value, and opaque alpha, all
/// little-endian.
#[inline]
pub(super) fn write_texel(bytes: &mut [u8], rgb: [f32; 3]) {
    for (slot, value) in bytes.chunks_exact_mut(2).zip(rgb) {
        let held = value.clamp(-HALF_MAX, HALF_MAX);
        slot.copy_from_slice(&half::f16::from_f32(held).to_bits().to_le_bytes());
    }
    bytes[6..8].copy_from_slice(&half::f16::ONE.to_bits().to_le_bytes());
}

/// One texel read back from the start of `bytes`.
#[inline]
pub(super) fn read_texel(bytes: &[u8]) -> [f32; 3] {
    std::array::from_fn(|channel| {
        half::f16::from_bits(u16::from_le_bytes([
            bytes[2 * channel],
            bytes[2 * channel + 1],
        ]))
        .to_f32()
    })
}

/// A rendered boundary: `rgba16float` texels, row by row, of the rectangle of the boundary layer's
/// received stage at `origin`.
#[derive(Clone, PartialEq, Eq)]
pub struct BoundaryFrame {
    /// Eight bytes a texel: red, green, blue and an opaque alpha, each a little-endian half float.
    pub texels: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// Where the first texel lies in the received stage.
    pub origin: (u32, u32),
    /// The whole stage the boundary layer receives, which the plan addresses.
    pub stage: Stage,
}

impl std::fmt::Debug for BoundaryFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundaryFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("origin", &self.origin)
            .field("stage", &self.stage)
            .finish_non_exhaustive()
    }
}

impl BoundaryFrame {
    /// The texel at `(x, y)` of the rectangle held, in linear light.
    pub fn texel(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        (x < self.width && y < self.height).then(|| {
            let offset = (y as usize * self.width as usize + x as usize) * TEXEL_BYTES;
            read_texel(&self.texels[offset..offset + TEXEL_BYTES])
        })
    }
}

impl Render<'_> {
    /// The input of the layer that begins at `position` — a segment and an operation index of
    /// `uncut` — as a boundary ([`BoundaryFrame`]).
    ///
    /// `self` renders `uncut` itself, or a windowed proxy's cut of it
    /// ([`super::window::WindowPlan::apply`]) over a source that holds `source_window` of
    /// `uncut`'s whole `source` stage. The boundary's frame is the segment's input exactly as a
    /// frame of this render builds it, and its pass is the segment's own over the operations
    /// before the layer, so every byte the CPU would hand the layer is the value held.
    pub(crate) fn boundary(
        &self,
        uncut: &Compiled,
        source: Stage,
        source_window: Region,
        position: (usize, usize),
    ) -> Result<BoundaryFrame, Error> {
        let (first, start) = position;
        let cut = &*self.compiled;
        if cut.segments.len() != uncut.segments.len() || first >= cut.segments.len() {
            return Err(Error::internal(
                "a GPU preview boundary was asked of another compilation",
            ));
        }
        let segment = &uncut.segments[first];
        if start > segment.operations.len() {
            return Err(Error::internal(
                "a GPU preview boundary lies past its segment",
            ));
        }
        // The whole stage the segment receives, and the rectangle of it this render's frame holds.
        let whole = match (first, &segment.entry) {
            (0, _) | (_, None) => source,
            (_, Some(entry)) => entry.stage(uncut.segments[first - 1].stage()),
        };
        let window = match (first, &cut.segments[first].entry) {
            (0, _) | (_, None) => source_window,
            (_, Some(Entry::Resample(entry))) => entry.window(),
            (_, Some(Entry::Spatial(_))) => {
                let previous = &cut.segments[first - 1];
                Region {
                    x0: previous.output_origin.0,
                    y0: previous.output_origin.1,
                    width: previous.width,
                    height: previous.height,
                }
            }
        };
        let stand_in = stand_in(&cut.segments[first], segment, start, whole, window)?;
        let received = {
            let mut before = ExactGeometry::identity(whole.width, whole.height);
            for operation in &segment.operations[..start] {
                if let Processing::ExactGeometry(step) = operation {
                    before = before.then(*step);
                }
            }
            Stage {
                width: before.output_width,
                height: before.output_height,
            }
        };
        let cancel = &self.options.cancel;
        let texels = match self.source {
            RenderSource::Byte(image) => {
                let (frame, stage) = super::byte::frames(
                    image,
                    cut,
                    cancel,
                    self.options.tiling,
                    self.context,
                    Some(first),
                    None,
                )?;
                super::byte::boundary_pass(&stand_in, &frame, stage, cancel, self.context)?
            }
            RenderSource::Linear { image, settings } => {
                let evaluation = super::Evaluation::frames_prefix(
                    linear::Linear::new(image, settings)?,
                    Cow::Borrowed(cut),
                    self.options.tiling,
                    cancel,
                    self.context,
                    first,
                )?;
                linear::boundary_pass(&evaluation, first, &stand_in, cancel, self.context)?
            }
        };
        cancel.check()?;
        Ok(BoundaryFrame {
            texels: Arc::new(texels),
            width: stand_in.width,
            height: stand_in.height,
            origin: stand_in.output_origin,
            stage: received,
        })
    }
}

/// The segment whose pass writes the boundary: `segment`'s operations before `start`, over the
/// frame `cut` reads — its entry, which holds `window` of the `whole` stage the segment receives —
/// kept to the rectangle of the received stage that window covers. The window is placed at its
/// origin ahead of the exact steps, and the crop to the rectangle held follows them as an exact
/// step of its own, so a masked operation maps its frame coordinate back to its mask's own pixel
/// exactly as a windowed proxy's cut segment does ([`super::window`]); the rectangle's origin is
/// the coordinate the pointwise units are handed.
fn stand_in(
    cut: &Segment,
    segment: &Segment,
    start: usize,
    whole: Stage,
    window: Region,
) -> Result<Segment, Error> {
    let mut operations: Vec<Processing> = segment.operations[..start].to_vec();
    let mut before = ExactGeometry::identity(whole.width, whole.height);
    for operation in &operations {
        if let Processing::ExactGeometry(step) = operation {
            before = before.then(*step);
        }
    }
    let received = Stage {
        width: before.output_width,
        height: before.output_height,
    };
    let held = before.map_region(window);
    if held.is_empty() {
        return Err(Error::internal(
            "a GPU preview boundary's window holds nothing of its stage",
        ));
    }
    let place = if window == Region::whole(whole) {
        ExactGeometry::identity(window.width, window.height)
    } else {
        ExactGeometry::crop(
            -i64::from(window.x0),
            -i64::from(window.y0),
            whole.width,
            whole.height,
        )
    };
    let mut geometry = place.then(before);
    if held != Region::whole(received) {
        let keep = ExactGeometry::crop(
            i64::from(held.x0),
            i64::from(held.y0),
            held.width,
            held.height,
        );
        geometry = geometry.then(keep);
        operations.push(Processing::ExactGeometry(keep));
    }
    if !geometry.reads_inside(window.width, window.height) {
        return Err(Error::internal(
            "a GPU preview boundary reads outside the frame its segment receives",
        ));
    }
    let has_pixels = operations
        .iter()
        .any(|operation| matches!(operation, Processing::PointReplace { .. }));
    let has_color = operations
        .iter()
        .any(|operation| matches!(operation, Processing::Color(_)));
    Ok(Segment {
        entry: cut.entry.clone(),
        operations,
        geometry,
        width: held.width,
        height: held.height,
        has_pixels,
        has_color,
        output_origin: (held.x0, held.y0),
    })
}
