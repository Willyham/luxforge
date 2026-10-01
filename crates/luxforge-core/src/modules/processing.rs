//! The closed host set of processing primitives a module may compile its payloads into.
//! Composition, mapping and rasterizing stay in the host; a module only describes its step.
//!
//! The neighbourhood primitive is large enough to live next door, in
//! [`spatial`](super::spatial): [`SpatialOperation`] and the [`SpatialUnit`](super::SpatialUnit)
//! trait it holds are re-exported through this module's parent alongside everything here.
use super::spatial::SpatialOperation;
use crate::mask_field::MaskField;
use crate::render::map::{Mapping, WarpStep};
use std::sync::Arc;

/// One image stage: the dimensions a layer's payload addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage {
    pub width: u32,
    pub height: u32,
}

/// Evaluated pixels per full-resolution content pixel, independently on each axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplingScale {
    pub x: f64,
    pub y: f64,
}

/// The evaluated stage and the full-resolution stage a layer addresses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileStage {
    pub stage: Stage,
    pub full: Stage,
    pub scale: SamplingScale,
}

impl CompileStage {
    pub fn exact(stage: Stage) -> Self {
        Self {
            stage,
            full: stage,
            scale: SamplingScale { x: 1.0, y: 1.0 },
        }
    }

    pub(crate) fn sampled(stage: Stage, full: Stage) -> Self {
        if stage == full {
            return Self::exact(stage);
        }
        Self {
            stage,
            full,
            scale: SamplingScale {
                x: f64::from(stage.width) / f64::from(full.width),
                y: f64::from(stage.height) / f64::from(full.height),
            },
        }
    }
}

/// An exact integer coordinate mapping with the stage it produces. Several of these compose into
/// one mapping, so a stack of transforms still rasterizes in a single pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactGeometry {
    pub a: i64,
    pub b: i64,
    pub c: i64,
    pub d: i64,
    pub tx: i64,
    pub ty: i64,
    pub output_width: u32,
    pub output_height: u32,
}

/// One interpolating boundary. `map` sends continuous output centres to input coordinates.
/// Bilinear taps are evaluated in linear light, with replicated edge indices. The host fuses
/// supported adjacent geometry into this boundary so it samples and quantizes only once.
#[derive(Clone, Debug, PartialEq)]
pub struct Resample {
    pub map: Mapping,
    pub output_width: u32,
    pub output_height: u32,
}

/// The largest number of pointwise units one compiled colour operation may hold. A module compiles
/// its whole payload into one operation, so this bounds the per-pixel work one layer can ask for.
pub(crate) const MAX_COLOR_UNITS: usize = 8;

/// One pointwise colour step, owned by the module that compiled it. The host decodes the frame into
/// linear sRGB (D65) f32 rows, hands each row to every unit in declared order and only then clamps,
/// encodes and quantizes, so a unit sees and may produce values outside `[0, 1]`: an inverse pair in
/// one operation round-trips exactly. A unit reads and writes colour channels only; alpha is the
/// host's and is never passed in.
///
/// A unit is pure and pointwise: `apply_row` must depend on nothing but the values it is given, the
/// position it is given them at and the unit's own coefficients, because the host chooses the row
/// chunking, applies the same unit on the shared Rayon pool and evaluates single pixels through the
/// same call for a point sample.
pub trait PointwiseColor: Send + Sync {
    /// Transform one row of linear-sRGB pixels in place, at row `y` of the stage the unit's segment
    /// produces, starting at column `x0`; the slice is always one contiguous run of that row, so
    /// pixel `i` is at `(x0 + i, y)`. A unit that does not depend on position ignores both. The
    /// rasterizing pass and every point query address the same coordinates, so a sample equals the
    /// rendered byte for a position-dependent unit too.
    fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]);
    /// Whether this unit's own coefficients are finite. Compilation refuses a unit that says no,
    /// so a non-finite parameter fails before a frame is touched.
    fn is_finite(&self) -> bool;
    /// A short, stable description of this unit and its coefficients. The host compares compiled
    /// operations by it, so two units that describe themselves identically must process identically:
    /// write every coefficient exactly, with the shortest round-trip form (`{}`), and never rounded
    /// to a display precision.
    fn describe(&self) -> String;
}

/// What one colour-stage layer compiles into: an ordered, bounded list of pointwise units evaluated
/// as one unbroken run, with no intermediate clamping or quantization between them, and the mask
/// the host modulates that run by.
///
/// The mask is the host's half of the primitive and a module never sets it: a module compiles its
/// payload into units, and [`ColorOperation::with_mask`] attaches the [`MaskField`] the layer's
/// own `mask` reference names. A masked operation is still one operation in its segment's ordered
/// list; what changes is that the host blends its output against **its own input**, per channel, in
/// linear light, before the run's single clamp and quantization
/// (`docs/design/masking.md`, "Masked colour and masked spatial").
#[derive(Clone, Default)]
pub struct ColorOperation {
    units: Vec<Arc<dyn PointwiseColor>>,
    mask: Option<MaskField>,
}

impl ColorOperation {
    /// An operation over these units in evaluation order. The host validates the count and the
    /// units' finiteness when it compiles the recipe, so a module may build one freely.
    pub fn new(units: Vec<Arc<dyn PointwiseColor>>) -> Self {
        Self { units, mask: None }
    }

    /// The same operation modulated by one compiled mask. Host-only: the mask comes from the
    /// layer's `mask` reference, which no module parses, plans or compiles.
    pub(crate) fn with_mask(mut self, mask: MaskField) -> Self {
        self.mask = Some(mask);
        self
    }

    /// The mask this operation is modulated by, or `None` for an operation that applies everywhere
    /// and therefore keeps today's exact byte path.
    pub(crate) fn mask(&self) -> Option<&MaskField> {
        self.mask.as_ref()
    }

    /// The operation a neutral payload compiles to: no units, which the host drops entirely, so a
    /// neutral layer keeps the identity byte path and the shared source buffer.
    pub(crate) fn neutral() -> Self {
        Self::default()
    }

    pub fn units(&self) -> &[Arc<dyn PointwiseColor>] {
        &self.units
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Whether every unit reports finite coefficients.
    pub(crate) fn is_finite(&self) -> bool {
        self.units.iter().all(|unit| unit.is_finite())
    }
}

/// Two operations are the same when their units describe themselves the same way in the same order
/// and they are modulated by the same compiled mask: a trait object carries no structural identity,
/// so the description is the comparison. A compiled mask is compared by allocation, which is
/// conservative — two separately compiled masks of equal payloads report unequal — because the
/// coverage field has no cheaper identity and nothing in the host depends on the other answer.
impl PartialEq for ColorOperation {
    fn eq(&self, other: &Self) -> bool {
        let masks = match (&self.mask, &other.mask) {
            (None, None) => true,
            (Some(left), Some(right)) => left.same_as(right),
            _ => false,
        };
        masks
            && self.units.len() == other.units.len()
            && std::iter::zip(&self.units, &other.units)
                .all(|(left, right)| left.describe() == right.describe())
    }
}

impl std::fmt::Debug for ColorOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut list = f.debug_list();
        list.entries(self.units.iter().map(|unit| unit.describe()));
        if let Some(mask) = &self.mask {
            list.entry(&format!(
                "masked by {} components over {}x{}{}",
                mask.components(),
                mask.stage().width,
                mask.stage().height,
                if mask.supersampled() {
                    ", supersampled 2x2"
                } else {
                    ""
                }
            ));
        }
        list.finish()
    }
}

/// What the host does with one compiled layer. `Eq` is not derivable because a resample carries
/// f64 coefficients, and `Copy` is not because a colour operation owns its units.
#[derive(Clone, Debug, PartialEq)]
pub enum Processing {
    ExactGeometry(ExactGeometry),
    /// One input-stage pixel, applied through the geometry that follows it.
    PointReplace {
        x: u32,
        y: u32,
        rgb: [u8; 3],
    },
    /// Pointwise colour over the whole stage, evaluated in linear-sRGB float by the host.
    Color(ColorOperation),
    /// A bounded neighbourhood of the whole stage, evaluated in linear-sRGB float by the host in
    /// tiles. Like a resample it is a stage boundary: it reads the frame the segment before it
    /// produced and writes the next one, at the same dimensions.
    Spatial(SpatialOperation),
    Resample(Resample),
    Warp(WarpStep),
}
