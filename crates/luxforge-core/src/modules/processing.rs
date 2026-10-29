//! The closed host set of processing primitives a module may compile its payloads into.
//! Composition, mapping and rasterizing stay in the host; a module only describes its step.
//!
//! The neighbourhood primitive is large enough to live next door, in
//! [`spatial`](super::spatial): [`SpatialOperation`] and the [`SpatialUnit`](super::SpatialUnit)
//! trait it holds are re-exported through this module's parent alongside everything here.
use super::repair::RepairTable;
use super::spatial::SpatialOperation;
use crate::mask_field::MaskField;
use crate::render::gpu::GpuDescription;
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
    /// Compile the layer in its GPU shape: every unit it can hold, a neutral one as its own
    /// identity, so its program sequence does not change as a value leaves or returns to neutral
    /// during a gesture. Only a GPU plan's drafted layer is compiled so
    /// (`crate::GpuPlanRequest::drafted`); every CPU compile leaves it unset, so no CPU frame,
    /// sample or answer ever sees a unit the CPU shape omits.
    pub(crate) gpu_shape: bool,
    /// The layer's input is scene-referred: a source-stage layer (a RAW photo's development) leads
    /// the stack, so a rendering Look follows rather than precedes it. Basic's scene unit applies
    /// plain gains there and its scene shift on a rendered source (`docs/design/basic-scene.md`).
    pub(crate) scene_referred: bool,
    /// On a scene-referred input, the scene white the layer receives: the development's
    /// ([`crate::Recipe::scene_white`]) through every global scene gain before it. `None` where the
    /// recipe is compiled without its source, for its stages or its structure only.
    pub(crate) scene_white: Option<f64>,
}

impl CompileStage {
    pub fn exact(stage: Stage) -> Self {
        Self {
            stage,
            full: stage,
            scale: SamplingScale { x: 1.0, y: 1.0 },
            gpu_shape: false,
            scene_referred: false,
            scene_white: None,
        }
    }

    /// The same stage over a scene-referred input when `scene_referred`
    /// ([`Self::scene_referred`]).
    pub(crate) fn over_scene(mut self, scene_referred: bool) -> Self {
        self.scene_referred = scene_referred;
        self
    }

    /// The same stage receiving the scene white `white` ([`Self::scene_white`]).
    pub(crate) fn white(mut self, white: Option<f64>) -> Self {
        self.scene_white = white;
        self
    }

    /// The same stage over a development whose scene white is `white`
    /// ([`crate::Recipe::scene_white`]): what a model compiled outside a stack, Auto tone's
    /// [`crate::forward_model`], is given for the stage it was read from. Only a scene-referred
    /// layer reads it.
    pub fn with_scene_white(self, white: Option<f64>) -> Self {
        self.white(white)
    }

    /// The same stage in the GPU shape ([`Self::gpu_shape`]) when `shaped`.
    pub(crate) fn shaped(mut self, shaped: bool) -> Self {
        self.gpu_shape = shaped;
        self
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
            gpu_shape: false,
            scene_referred: false,
            scene_white: None,
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

/// Exact processing identity, independent of diagnostic text. Each unit names its kind and
/// supplies every coefficient and stage-dependent value as exact bits in a fixed layout. Variable
/// sequences include their lengths. This is neither a digest nor a persisted compatibility format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationIdentity {
    kind: &'static str,
    state: Vec<u64>,
}

impl OperationIdentity {
    pub fn new(kind: &'static str, state: impl IntoIterator<Item = u64>) -> Self {
        Self {
            kind,
            state: state.into_iter().collect(),
        }
    }

    pub(crate) fn with_words(mut self, words: impl IntoIterator<Item = u64>) -> Self {
        let start = self.state.len();
        self.state.push(0);
        self.state.extend(words);
        self.state[start] = (self.state.len() - start - 1) as u64;
        self
    }
}

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
    /// [`Self::apply_row`] for a unit whose arithmetic can fail: an external module's, which runs
    /// in a sandbox that may trap, overrun its deadline or answer the wrong length. The host's
    /// colour runs call this, so such a failure fails the render, sample or export with the
    /// unit's own error, which names the module, and no frame is produced. The default runs
    /// `apply_row` and never fails, which is every linked module's unit. A unit that overrides
    /// this still fills the run with non-finite values from `apply_row` when it fails there, so a
    /// caller of `apply_row` refuses its result too.
    fn try_apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) -> Result<(), crate::Error> {
        self.apply_row(y, x0, rgb);
        Ok(())
    }
    /// Whether this unit's own coefficients are finite. Compilation refuses a unit that says no,
    /// so a non-finite parameter fails before a frame is touched.
    fn is_finite(&self) -> bool;
    /// Exact unit kind, coefficients and relevant stage state. No rendering or lazy table build.
    fn identity(&self) -> OperationIdentity;
    /// A diagnostic description. Its wording and formatting do not affect operation equality.
    fn describe(&self) -> String;
    /// This unit's GPU program and the uniform words it reads for display, histogram analysis,
    /// samples, catalog tiers and export (`docs/design/gpu-first.md`). The module keeps the WGSL
    /// beside this unit; its words follow the unit's exact processing state. `None`, the default,
    /// sends a stack needing this unit to the whole-frame reference renderer, also used on a
    /// host without a usable GPU. GPU outputs meet the declared tolerance against that reference;
    /// the independent CPU implementation remains the exact-buffer reference.
    fn gpu(&self) -> Option<GpuDescription> {
        None
    }
    /// Whether this unit, compiled for a stage reduced from full resolution, draws a stand-in
    /// there for its full-resolution output rather than that output at the reduced size, as
    /// grain's reduced-stage field, matched to the full-resolution field in its statistics alone,
    /// does. `false`, the default: a unit whose equations are normalized to the stage, as every
    /// pointwise colour unit and the vignette's are, draws the same picture at any size.
    fn stands_in_when_reduced(&self) -> bool {
        false
    }
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
    /// The units of the layer's **rendered tail**, which on a scene-referred stack the host places
    /// directly after the stack's rendering layer (the Look) when one follows this layer, and
    /// otherwise directly after `units` (`docs/design/basic-local-tone.md`, "One layer, two
    /// places"). The same mask modulates both. Empty for every module but Basic.
    tail: Vec<Arc<dyn PointwiseColor>>,
    /// The factor by which this layer, where it applies everywhere, scales the scene white of every
    /// later layer: Basic's white balance and Exposure, plain gains on a scene-referred input.
    scene_gain: Option<f64>,
}

impl ColorOperation {
    /// An operation over these units in evaluation order. The host validates the count and the
    /// units' finiteness when it compiles the recipe, so a module may build one freely.
    pub fn new(units: Vec<Arc<dyn PointwiseColor>>) -> Self {
        Self {
            units,
            ..Self::default()
        }
    }

    /// The same operation with `tail` as its rendered tail ([`Self::tail`]).
    pub(crate) fn with_tail(mut self, tail: Vec<Arc<dyn PointwiseColor>>) -> Self {
        self.tail = tail;
        self
    }

    /// The same operation scaling later layers' scene white by `gain` ([`Self::scene_gain`]).
    pub(crate) fn with_scene_gain(mut self, gain: f64) -> Self {
        self.scene_gain = Some(gain);
        self
    }

    /// The rendered tail's units, in evaluation order.
    pub(crate) fn tail(&self) -> &[Arc<dyn PointwiseColor>] {
        &self.tail
    }

    /// The factor this layer scales later layers' scene white by; 1 for an operation that declares
    /// none.
    pub(crate) fn scene_gain(&self) -> f64 {
        self.scene_gain.unwrap_or(1.0)
    }

    /// This operation's rendered tail as an operation of its own, under the same mask, or `None`
    /// when it has none; and this operation without it.
    pub(crate) fn split_tail(mut self) -> (Self, Option<Self>) {
        let tail = std::mem::take(&mut self.tail);
        let rest = (!tail.is_empty()).then(|| Self {
            units: tail,
            mask: self.mask.clone(),
            tail: Vec::new(),
            scene_gain: None,
        });
        (self, rest)
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

    /// Whether the operation holds no unit, in its body or its tail.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty() && self.tail.is_empty()
    }

    /// Whether every unit, of the body and the tail, reports finite coefficients.
    pub(crate) fn is_finite(&self) -> bool {
        self.units
            .iter()
            .chain(&self.tail)
            .all(|unit| unit.is_finite())
            && self
                .scene_gain
                .is_none_or(|gain| gain.is_finite() && gain > 0.0)
    }
}

/// Equality compares exact unit identities in order and the same compiled mask allocation.
/// Mask equality remains conservative: separately compiled equal payloads need not share a field.
impl PartialEq for ColorOperation {
    fn eq(&self, other: &Self) -> bool {
        let masks = match (&self.mask, &other.mask) {
            (None, None) => true,
            (Some(left), Some(right)) => left.same_as(right),
            _ => false,
        };
        let same = |left: &[Arc<dyn PointwiseColor>], right: &[Arc<dyn PointwiseColor>]| {
            left.len() == right.len()
                && std::iter::zip(left, right).all(|(l, r)| l.identity() == r.identity())
        };
        masks
            && same(&self.units, &other.units)
            && same(&self.tail, &other.tail)
            && self.scene_gain.map(f64::to_bits) == other.scene_gain.map(f64::to_bits)
    }
}

impl std::fmt::Debug for ColorOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut list = f.debug_list();
        list.entries(self.units.iter().map(|unit| unit.describe()));
        if !self.tail.is_empty() {
            list.entry(&format!(
                "rendered tail {:?}",
                self.tail
                    .iter()
                    .map(|unit| unit.describe())
                    .collect::<Vec<_>>()
            ));
        }
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
    /// Pointwise colour over the whole stage, evaluated in linear-sRGB float by the host.
    Color(ColorOperation),
    /// A bounded neighbourhood of the whole stage, evaluated in linear-sRGB float by the host in
    /// tiles. Like a resample it is a stage boundary: it reads the frame the segment before it
    /// produced and writes the next one, at the same dimensions.
    Spatial(SpatialOperation),
    /// The repair stage's ordered Clone and Heal operations ([`RepairTable`]), each reading a
    /// bounded region of its input that may lie anywhere in the stage. A stage boundary like a
    /// spatial operation: it reads the frame the segment before it produced and writes the next
    /// one, at the same dimensions. Only a `repair`-stage effect compiles to it.
    Repair(RepairTable),
    Resample(Resample),
    Warp(WarpStep),
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[derive(Clone)]
    struct Unit {
        kind: &'static str,
        gain: f64,
        width: u32,
        description: &'static str,
    }
    impl PointwiseColor for Unit {
        fn apply_row(&self, _: u32, x0: u32, rgb: &mut [[f32; 3]]) {
            for (x, pixel) in rgb.iter_mut().enumerate() {
                pixel[0] = pixel[0] * self.gain as f32 + (x0 as f32 + x as f32) / self.width as f32;
            }
        }
        fn is_finite(&self) -> bool {
            self.gain.is_finite()
        }
        fn identity(&self) -> OperationIdentity {
            OperationIdentity::new(self.kind, [self.gain.to_bits(), u64::from(self.width)])
        }
        fn describe(&self) -> String {
            self.description.into()
        }
    }
    fn operation(unit: Unit) -> ColorOperation {
        ColorOperation::new(vec![Arc::new(unit)])
    }
    fn base() -> Unit {
        Unit {
            kind: "test.gain",
            gain: 1.,
            width: 10,
            description: "old wording",
        }
    }

    #[test]
    fn diagnostic_wording_can_change_without_changing_equality_or_pixels() {
        let left = base();
        let right = Unit {
            description: "new wording",
            ..left.clone()
        };
        assert_ne!(left.describe(), right.describe());
        let mut a = [[0.1, 0.2, 0.3]; 2];
        let mut b = a;
        left.apply_row(0, 2, &mut a);
        right.apply_row(0, 2, &mut b);
        assert_eq!(a, b);
        assert_eq!(operation(left), operation(right));
    }

    #[test]
    fn exact_bits_kind_stage_and_unit_order_are_distinct() {
        let a = base();
        for b in [
            Unit {
                kind: "test.other",
                ..a.clone()
            },
            Unit {
                gain: f64::from_bits(a.gain.to_bits() + 1),
                ..a.clone()
            },
            Unit {
                width: 11,
                ..a.clone()
            },
        ] {
            assert_eq!(a.describe(), b.describe());
            assert_ne!(operation(a.clone()), operation(b));
        }
        let b = Unit {
            gain: 2.,
            ..a.clone()
        };
        assert_ne!(
            ColorOperation::new(vec![Arc::new(a.clone()), Arc::new(b.clone())]),
            ColorOperation::new(vec![Arc::new(b), Arc::new(a)])
        );
        let nan = Unit {
            gain: f64::from_bits(0x7ff8_0000_0000_0001),
            ..base()
        };
        assert_eq!(operation(nan.clone()), operation(nan));
    }

    #[test]
    fn variable_sections_have_explicit_lengths() {
        let a = OperationIdentity::new("test.sections", [])
            .with_words([1, 2])
            .with_words([3]);
        let b = OperationIdentity::new("test.sections", [])
            .with_words([1])
            .with_words([2, 3]);
        assert_ne!(a, b);
    }
}
