//! Colour runs: the maximal spans of colour operations a segment evaluates as one float pass, their
//! masked blend and the chunk size a rasterizing pass streams them in.

use super::Segment;
use crate::{
    Error,
    mask_field::MaskField,
    modules::{ColorOperation, ExactGeometry, Processing, Region, Stage},
};

/// A frame row chunk holds at most this many bytes of float scratch, so the peak is the worker
/// count times this and nothing scales with the image. Rayon runs at most one chunk per worker:
/// 16 workers on the host's M4 Pro reach 16 MiB, comfortably inside the 64 MiB budget, and a
/// 16384-pixel row — the widest side the host accepts — still leaves 5 rows per chunk.
pub(super) const COLOR_CHUNK_SCRATCH_BYTES: usize = 1024 * 1024;

/// And never more rows than this, so a narrow image does not buffer an arbitrary slice of the
/// frame: 16 rows of a 10000-pixel image is 16 × 10000 × 12 = 1.92 MB of `[f32; 3]`, which is why
/// the byte cap above decides there and the row cap decides for narrow frames.
pub(super) const COLOR_CHUNK_ROWS: usize = 16;

/// The rows of one streamed colour chunk at this frame width.
pub(super) fn color_chunk_rows(width: u32) -> usize {
    let row = (width as usize).max(1) * std::mem::size_of::<[f32; 3]>();
    (COLOR_CHUNK_SCRATCH_BYTES / row).clamp(1, COLOR_CHUNK_ROWS)
}

pub(super) const NON_FINITE_COLOR: &str = "colour processing produced a non-finite value";

/// One maximal run of colour operations in a segment's operation list: every operation between two
/// point replacements, which is the span the host evaluates as one unbroken float pass. Exact
/// geometry is already composed into the segment's single raster pass and commutes with pointwise
/// colour, so it does not break a run; a point replacement does, because a replacement written
/// before a run is processed by it and one written after it is not.
#[derive(Clone, Copy)]
pub(crate) struct ColorRun<'a> {
    /// The index of this run's first colour operation in the segment's operation list.
    pub(super) start: usize,
    /// The index of this run's last colour operation, inclusive.
    end: usize,
    /// The **whole** segment's operation list, not just this run's span. A masked operation is
    /// handed a coordinate of the frame this run colours, which is the segment's output frame, so it
    /// needs the exact geometry that follows it — inside this run's span and after it — to map that
    /// coordinate back to the stage its own mask was compiled against.
    operations: &'a [Processing],
    /// The segment's output stage: the frame this run is applied to.
    stage: Stage,
    /// This segment's cut output lies here in its original, uncut stage. Masks still read local
    /// coordinates through their geometry suffix; only pointwise units receive this origin.
    origin: (u32, u32),
}

impl<'a> ColorRun<'a> {
    /// This run's colour operations in evaluation order, each with its index in the segment's
    /// operation list.
    #[inline]
    pub(super) fn colour_operations(self) -> impl Iterator<Item = (usize, &'a ColorOperation)> {
        self.operations[self.start..=self.end]
            .iter()
            .enumerate()
            .filter_map(move |(offset, operation)| match operation {
                Processing::Color(operation) => Some((self.start + offset, operation)),
                _ => None,
            })
    }

    pub(super) fn followed_by_replace(self) -> bool {
        self.operations[self.end + 1..]
            .iter()
            .any(|op| matches!(op, Processing::PointReplace { .. }))
    }

    /// Whether any operation of this run is modulated by a mask, which is what decides whether the
    /// pass needs snapshot scratch at all. An unmasked run costs and allocates exactly what it did
    /// before masks existed.
    pub(super) fn has_mask(self) -> bool {
        self.colour_operations()
            .any(|(_, operation)| operation.mask().is_some())
    }
}

#[derive(Clone)]
pub(super) struct ColorRuns<'a> {
    operations: &'a [Processing],
    stage: Stage,
    origin: (u32, u32),
    position: usize,
}

impl<'a> Iterator for ColorRuns<'a> {
    type Item = ColorRun<'a>;

    #[inline]
    fn next(&mut self) -> Option<ColorRun<'a>> {
        while self.position < self.operations.len() {
            if !matches!(self.operations[self.position], Processing::Color(_)) {
                self.position += 1;
                continue;
            }
            let start = self.position;
            let mut last = start;
            for (index, operation) in self.operations.iter().enumerate().skip(start) {
                match operation {
                    Processing::Color(_) => last = index,
                    Processing::PointReplace { .. } => break,
                    Processing::ExactGeometry(_)
                    | Processing::Spatial(_)
                    | Processing::Resample(_)
                    | Processing::Warp(_) => {}
                }
            }
            self.position = last + 1;
            return Some(ColorRun {
                start,
                end: last,
                operations: self.operations,
                stage: self.stage,
                origin: self.origin,
            });
        }
        None
    }
}

#[inline]
pub(super) fn color_runs(segment: &Segment) -> ColorRuns<'_> {
    ColorRuns {
        operations: &segment.operations,
        stage: segment.stage(),
        origin: segment.output_origin,
        position: 0,
    }
}

/// One masked operation's mask, placed in the frame the run is applied to.
///
/// A mask is compiled against the stage its layer receives — the content stage, for every layer that
/// may carry one — while a colour run is applied to the frame its segment produces, after the
/// segment's exact geometry has been composed into one pass. The two differ by exactly the exact
/// steps that follow the operation, which is the same suffix a point replacement is mapped through
/// ([performance rule 3](../../../../docs/engineering/performance-rules.md)). Composing that suffix costs
/// `O(operations)` integer multiplies, allocates nothing, and is exact: `a`, `b`, `c`, `d` are a
/// signed permutation, so `unmap` is the mapping's exact inverse and a mask lands on the same content
/// pixels through a quarter turn, a reflection and an axis-aligned crop.
struct MaskPlacement<'a> {
    mask: &'a MaskField,
    /// From the stage the mask was compiled against to the frame this run colours.
    suffix: ExactGeometry,
    /// [`MaskField::bounds`] mapped into that frame: outside it coverage is exactly zero, so the
    /// blend is the identity there and no unit is evaluated at all.
    bounds: Region,
}

impl<'a> MaskPlacement<'a> {
    fn new(run: &ColorRun<'a>, index: usize, mask: &'a MaskField) -> Self {
        let mut suffix = ExactGeometry::identity(run.stage.width, run.stage.height);
        for operation in run.operations[index + 1..].iter().rev() {
            if let Processing::ExactGeometry(step) = operation {
                suffix = step.then(suffix);
            }
        }
        Self {
            mask,
            bounds: suffix.map_region(mask.bounds()),
            suffix,
        }
    }

    /// The coverage at one frame pixel: the mask's own field at the pixel of its own stage that this
    /// frame pixel came from, for `input` — **the value this operation receives at that pixel**, in
    /// linear float, which is the snapshot the blend is taken against and not the value the units
    /// have produced from it. The rasterizing pass and `render.sample` reach this through the same
    /// call with the same arguments, so a sampled byte equals the rendered byte for a masked
    /// layer by construction, for a value-based component exactly as for a geometric one.
    fn coverage(&self, x: u32, y: u32, input: [f32; 3]) -> f32 {
        let (mask_x, mask_y) = self.suffix.unmap(x, y);
        self.mask.evaluate(mask_x, mask_y, input)
    }

    /// The part of one contiguous row span this mask can reach, as offsets into that span, or `None`
    /// when the span lies entirely outside the bounds rectangle.
    fn span(&self, y: u32, x0: u32, len: usize) -> Option<(usize, usize)> {
        if self.bounds.is_empty() || y < self.bounds.y0 || y >= self.bounds.y1() {
            return None;
        }
        let last = x0.saturating_add(len as u32);
        let from = self.bounds.x0.max(x0);
        let to = self.bounds.x1().min(last);
        if to <= from {
            return None;
        }
        Some(((from - x0) as usize, (to - x0) as usize))
    }
}

/// One pixel of a masked operation's scratch: the value the operation receives there, which its
/// blend is taken against, and the mask's coverage of it. Coverage reads the pixel's position and
/// that value alone, so both are known before any unit runs, and together they decide whether the
/// units run at that pixel at all ([`MaskedInput::keeps_input`]).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MaskedInput {
    input: [f32; 3],
    coverage: f32,
}

impl MaskedInput {
    /// Whether the blend hands this pixel back its input bit for bit whatever finite value the
    /// units produce there, so they need not run.
    ///
    /// At coverage `M = ±0.0` the blend computes `(1 − M)·in + M·u`. `1 − M` is exactly `1.0`, and
    /// `1.0·in` is exactly `in`; `M·u` is a zero for any finite `u`, and adding a zero to `in`
    /// returns `in` exactly — except where `in` is `−0.0`, since `−0.0 + +0.0` is `+0.0` and which
    /// zero `M·u` is depends on the sign of the `u` a skipped pixel never computes. So a pixel keeps
    /// its input when its coverage is a zero of either sign and every channel of its input is
    /// finite and not `−0.0`, the condition a masked spatial tile is copied on. Every other pixel,
    /// one whose coverage is not a number included, runs the units and is blended as it always was.
    #[inline]
    fn keeps_input(&self) -> bool {
        self.coverage == 0.0
            && self
                .input
                .iter()
                .all(|value| value.is_finite() && value.to_bits() != (-0.0_f32).to_bits())
    }
}

/// Apply one run to one contiguous run of already decoded linear pixels of row `y` starting at
/// column `x0`, in the coordinates of the stage its segment produces. Nothing is clamped or
/// quantized between operations or between units, so an inverse pair returns its input exactly and a
/// value outside `[0, 1]` survives to the next unit.
///
/// The run is walked **operation by operation** rather than as one flat stream of units, because a
/// masked operation is blended against its own input and an unmasked one is not. An unmasked
/// operation is applied exactly as before: every unit in order, with the finite check over the whole
/// slice after each unit rather than per pixel inside it, so the hot loop stays branch-free and the
/// run fails as soon as a unit has produced a non-finite value. Flattening the units of consecutive
/// unmasked operations into one stream, which is what this used to do, produces the same arithmetic
/// in the same order.
///
/// `scratch` is the snapshot a masked operation takes of the pixels it is handed, one
/// [`MaskedInput`] each: the pixel's input, which the blend is against, and its coverage. Its length
/// is free: the units are pointwise, so a masked span is processed in blocks of at most
/// `scratch.len()` pixels and the result does not depend on the block size. The rasterizing pass
/// hands it one row of float scratch reserved from the budget; a point query hands it one pixel on
/// the stack and allocates nothing.
#[inline]
pub(super) fn apply_units(
    run: &ColorRun<'_>,
    y: u32,
    x0: u32,
    pixels: &mut [[f32; 3]],
    scratch: &mut [MaskedInput],
) -> Result<(), Error> {
    for (index, operation) in run.colour_operations() {
        match operation.mask() {
            None => apply_operation(operation, y, x0, run.origin, pixels)?,
            Some(mask) => {
                let placement = MaskPlacement::new(run, index, mask);
                apply_masked_operation(operation, &placement, y, x0, run.origin, pixels, scratch)?;
            }
        }
    }
    Ok(())
}

/// Every unit of one operation, in order, over the whole slice it is given.
#[inline]
fn apply_operation(
    operation: &ColorOperation,
    y: u32,
    x0: u32,
    origin: (u32, u32),
    pixels: &mut [[f32; 3]],
) -> Result<(), Error> {
    for unit in operation.units() {
        unit.apply_row(y + origin.1, x0 + origin.0, pixels);
        // The conjunction over every channel, without the short circuit `all` would take: the
        // answer is the same, and a scan that never exits early vectorises.
        let finite = pixels
            .as_flattened()
            .iter()
            .fold(true, |finite, channel| finite & channel.is_finite());
        if !finite {
            return Err(Error::resource_limit(NON_FINITE_COLOR));
        }
    }
    Ok(())
}

/// One masked operation over one contiguous row span: `out = (1 − M)·in + M·units(in)` per channel,
/// in linear float, inside the run.
///
/// Four properties are load-bearing and are what the tests assert:
///
/// - **The blend is against the operation's own input**, so an unmasked operation before or after it
///   in the same run is unaffected and nothing is clamped or quantized in between. That is why the
///   input is snapshotted rather than recomputed.
/// - **The endpoints are exact.** `M = 0` leaves `1·in + 0·units(in)`, which is `in`, and `M = 1`
///   leaves `0·in + 1·units(in)`, which is `units(in)` — both bit for bit, which the algebraically
///   equal `in + M·(units(in) − in)` is not. Only the sign of a zero can change, and a signed zero
///   quantizes to the same code.
/// - **Outside the bounds rectangle nothing is evaluated at all**, not merely blended away, so a
///   small mask on a large frame costs the units of its own rectangle and no more.
/// - **Inside it, no unit runs where the coverage is zero.** A block's coverage is evaluated first,
///   on its snapshot, and the units then run over each maximal stretch of pixels that does not keep
///   its input ([`MaskedInput::keeps_input`]), each stretch handed its own first column, so a
///   position-dependent unit is handed the coordinates it always was. A pixel that keeps its input
///   is left as it is, which is the bit the blend would have written there. So a mask whose
///   rectangle is the whole stage — a range, or any inverted component — costs a coverage
///   evaluation per pixel and the units of the pixels it selects.
///
/// What skipping changes is a value no output shows: a unit that would have produced a non-finite
/// value at a pixel whose coverage is zero is never run there, so it cannot fail the render, as was
/// already true outside the bounds rectangle. A point sample takes the same decision for its one
/// pixel through this function, so a sampled byte still equals the rendered byte, and a sample of a
/// pixel whose units fail the render fails as the render does.
fn apply_masked_operation(
    operation: &ColorOperation,
    placement: &MaskPlacement<'_>,
    y: u32,
    x0: u32,
    origin: (u32, u32),
    pixels: &mut [[f32; 3]],
    scratch: &mut [MaskedInput],
) -> Result<(), Error> {
    let Some((from, to)) = placement.span(y, x0, pixels.len()) else {
        return Ok(());
    };
    debug_assert!(!scratch.is_empty(), "a masked run needs snapshot scratch");
    let rule = unit_skip();
    let block = scratch.len().max(1);
    let mut at = from;
    while at < to {
        let end = (at + block).min(to);
        let span = &mut pixels[at..end];
        let snapshot = &mut scratch[..span.len()];
        let mut uncovered = false;
        for (offset, (held, input)) in snapshot.iter_mut().zip(span.iter()).enumerate() {
            let coverage = placement.coverage(x0 + (at + offset) as u32, y, *input);
            uncovered |= coverage == 0.0;
            *held = MaskedInput {
                input: *input,
                coverage,
            };
        }
        // A block the mask covers throughout is one stretch, with nothing to look for in it.
        let skips = uncovered && rule == UnitSkip::Proved;
        let evaluated = |pixel: &MaskedInput| !(skips && pixel.keeps_input());
        let mut start = 0;
        while let Some(skipped) = snapshot[start..].iter().position(evaluated) {
            let first = start + skipped;
            let last = snapshot[first..]
                .iter()
                .position(|pixel| !evaluated(pixel))
                .map_or(span.len(), |length| first + length);
            let stretch = &mut span[first..last];
            apply_operation(operation, y, x0 + (at + first) as u32, origin, stretch)?;
            for (output, held) in stretch.iter_mut().zip(&snapshot[first..last]) {
                for (value, input) in output.iter_mut().zip(held.input) {
                    *value = (1.0 - held.coverage) * input + held.coverage * *value;
                }
            }
            start = last;
        }
        at = end;
    }
    Ok(())
}

/// Which pixels inside a masked operation's bounds run its units. Production runs them only at the
/// pixels that do not keep their input ([`MaskedInput::keeps_input`]). A test chooses
/// [`UnitSkip::Never`] to run them at every pixel inside the bounds, which is what the skip is
/// compared against bit for bit, and a measurement chooses it for the rule before the skip existed.
/// Choosing it changes no value a render produces, only how many pixels the units run over, and so
/// whether a non-finite value they produce at a pixel whose coverage is zero fails the render.
///
/// The choice belongs to the thread that runs the row, as a masked spatial tile's copy rule does
/// (`spatial::TileCopy`): a pass below the parallel threshold, or one forced serial, runs every row
/// on the thread that asked for the frame, and a measurement at photo size sets the rule on every
/// pool thread as well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum UnitSkip {
    Never,
    Proved,
}

#[cfg(not(test))]
const fn unit_skip() -> UnitSkip {
    UnitSkip::Proved
}

#[cfg(test)]
fn unit_skip() -> UnitSkip {
    UNIT_SKIP.get()
}

#[cfg(test)]
thread_local! {
    static UNIT_SKIP: std::cell::Cell<UnitSkip> = const { std::cell::Cell::new(UnitSkip::Proved) };
}

/// Set which pixels this thread's masked colour operations skip, for a test; returns the previous
/// rule.
#[cfg(test)]
pub(super) fn set_unit_skip(skip: UnitSkip) -> UnitSkip {
    UNIT_SKIP.replace(skip)
}
