//! The neighbourhood-dependent processing primitive: what a spatial-stage module declares and what
//! the host guarantees around it.
//!
//! A module compiles its payload into one [`SpatialOperation`], an ordered list of at most
//! [`MAX_SPATIAL_UNITS`] units. The host owns everything else: the stage boundary, the tiling, the
//! halo bookkeeping, the scratch, the scheduling, the global estimate and the point-sample path.
//! Nothing here reads a frame or allocates one; the execution side lives in
//! [`crate::render`](crate::render).
use crate::{Error, ErrorKind, mask::CompiledMask, modules::Stage};
use std::sync::Arc;

/// The side of one output tile the host streams. The stage is covered by tiles of this size
/// anchored at the stage origin, with partial tiles at the right and bottom edges.
pub const SPATIAL_TILE: u32 = 512;

/// The largest summed halo, in input pixels, one operation may declare at a stage. An operation
/// that needs more is refused at compile time; a halo is never silently reduced.
pub const MAX_SPATIAL_HALO: u32 = 512;

/// The largest number of units one compiled spatial operation may hold. A module compiles its whole
/// payload into one operation, so this bounds the chain one layer can ask the host to run.
pub const MAX_SPATIAL_UNITS: usize = 4;

/// The largest number of **masked** spatial layers one recipe may hold
/// (`docs/design/masking.md`, "Limits").
///
/// Every spatial layer, masked or not, is a stage boundary and therefore a sequential full frame —
/// the operation reads the finished frame before it and writes the next one, so no two of them
/// overlap in time. A mask does not change that; what a mask changes is how cheap the frame is,
/// because a tile the mask cannot reach is copied instead of evaluated. The cap is on the masked
/// ones because local adjustments are the gesture that invites many of them, and four sequential
/// full frames is what the measurement in `docs/specs/performance.md` was taken against. Exceeding
/// it is a `resource-limit` error naming the limit, not a silently dropped layer.
pub const MAX_MASKED_SPATIAL_LAYERS: usize = 4;

/// The default process-wide bound on spatial working sets: 256 MiB, separate from the 64 MiB float
/// scratch budget the colour run streams through, because one tile of a 60 MP stage with the frozen
/// presence halos needs about 57 MiB on its own.
pub const SPATIAL_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// The per-side factor of the reduction a global estimate is prepared from. The reduced frame is at
/// most [`MAX_REDUCTION_PIXELS`] pixels, because the host accepts at most 64 megapixels.
pub const ESTIMATE_REDUCTION: u32 = 16;

/// The largest reduced frame [`Reduction`] will build: 2^18 pixels, which is the 0.25 megapixel
/// bound the design states. A 64 MP stage reduced by 16 per side rounds up to at most 250,880
/// pixels, so this bound is never the binding constraint on a stage the host accepts.
pub const MAX_REDUCTION_PIXELS: u64 = 262_144;

/// The largest global estimate a unit may return, in bytes.
pub const MAX_GLOBAL_BYTES: usize = 4096;

/// The largest number of `f64` values that fits [`MAX_GLOBAL_BYTES`].
pub const MAX_GLOBAL_VALUES: usize = MAX_GLOBAL_BYTES / std::mem::size_of::<f64>();

/// How many prepared global estimates the host keeps, evicted oldest first.
pub const ESTIMATE_STORE_ENTRIES: usize = 8;

/// A rectangle of one stage, in that stage's pixel coordinates. Half-open: it holds the columns
/// `x0..x0 + width` and the rows `y0..y0 + height`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// The column after the last one this region holds.
    pub fn x1(self) -> u32 {
        self.x0.saturating_add(self.width)
    }

    /// The row after the last one this region holds.
    pub fn y1(self) -> u32 {
        self.y0.saturating_add(self.height)
    }

    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// How many pixels this region holds. One plane of `f32` values is four times this many bytes.
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// The bytes three `f32` planes of this region occupy.
    pub fn plane_bytes(self) -> u64 {
        self.pixels() * 3 * std::mem::size_of::<f32>() as u64
    }

    pub fn contains(self, x: u32, y: u32) -> bool {
        (self.x0..self.x1()).contains(&x) && (self.y0..self.y1()).contains(&y)
    }

    /// This region grown by `halo` pixels on every side and clamped to the stage.
    pub fn grown(self, halo: u32, stage: Stage) -> Self {
        let x0 = self.x0.saturating_sub(halo);
        let y0 = self.y0.saturating_sub(halo);
        let x1 = self.x1().saturating_add(halo).min(stage.width);
        let y1 = self.y1().saturating_add(halo).min(stage.height);
        Self {
            x0,
            y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        }
    }

    /// The rectangle a unit of this halo fills when it is given `self` as its input rectangle: the
    /// input shrunk by `halo` on every side **except** a side that lies on the stage edge, where
    /// there is nothing further to read and the unit produces output up to the edge.
    ///
    /// This is the one rule that decides what a unit must write, and the host builds its output
    /// planes from exactly this function, so a unit that computes the rectangle the same way can
    /// never disagree with the buffer it is handed.
    pub fn shrunk(self, halo: u32, stage: Stage) -> Self {
        let x0 = if self.x0 == 0 {
            0
        } else {
            self.x0.saturating_add(halo)
        };
        let y0 = if self.y0 == 0 {
            0
        } else {
            self.y0.saturating_add(halo)
        };
        let x1 = if self.x1() >= stage.width {
            stage.width
        } else {
            self.x1().saturating_sub(halo)
        };
        let y1 = if self.y1() >= stage.height {
            stage.height
        } else {
            self.y1().saturating_sub(halo)
        };
        Self {
            x0,
            y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        }
    }
}

/// The bounded reduction of an operation's input stage a global estimate is prepared from: a
/// `1/ESTIMATE_REDUCTION`-per-side box average in planar `f32` linear-sRGB RGB.
///
/// Reduced pixel `(i, j)` is the mean of the input pixels in
/// `[i·s, min((i+1)·s, width)) × [j·s, min((j+1)·s, height))`, so a partial block at the right or
/// bottom edge is averaged over its actual pixels and never over clamped copies. The block grid is
/// anchored at the stage origin, which is what makes a reduced pixel the same value whatever tile
/// or sample asked for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Reduction {
    stage: Stage,
    factor: u32,
    width: u32,
    height: u32,
    /// One complete R plane, then G, then B.
    values: Vec<f32>,
}

impl Reduction {
    /// The reduced dimensions of a stage at this factor.
    pub(crate) fn dimensions(stage: Stage, factor: u32) -> (u32, u32) {
        let factor = factor.max(1);
        (stage.width.div_ceil(factor), stage.height.div_ceil(factor))
    }

    /// Adopt already-reduced planar values. The host builds these; a module only reads them.
    pub(crate) fn new(stage: Stage, factor: u32, values: Vec<f32>) -> Result<Self, Error> {
        let (width, height) = Self::dimensions(stage, factor);
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_REDUCTION_PIXELS {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "a spatial reduction of {pixels} pixels exceeds the {MAX_REDUCTION_PIXELS} pixel bound"
                ),
            ));
        }
        if values.len() as u64 != pixels * 3 {
            return Err(Error::new(
                ErrorKind::Internal,
                "a spatial reduction needs three complete planes",
            ));
        }
        Ok(Self {
            stage,
            factor: factor.max(1),
            width,
            height,
            values,
        })
    }

    /// The stage this reduction was built from.
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The per-side reduction factor.
    pub fn factor(&self) -> u32 {
        self.factor
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// One reduced plane: `0` is red, `1` green, `2` blue, in row-major order.
    pub fn plane(&self, channel: usize) -> &[f32] {
        let len = (u64::from(self.width) * u64::from(self.height)) as usize;
        &self.values[channel * len..(channel + 1) * len]
    }

    /// One reduced pixel, or `None` outside the reduced frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let len = (u64::from(self.width) * u64::from(self.height)) as usize;
        let index = (u64::from(y) * u64::from(self.width) + u64::from(x)) as usize;
        Some([
            self.values[index],
            self.values[len + index],
            self.values[2 * len + index],
        ])
    }
}

/// A small owned value a unit prepares once from a [`Reduction`] and every tile reads.
///
/// It is bounded to [`MAX_GLOBAL_BYTES`], so a global estimate can never grow with the image: an
/// atmospheric light is three numbers, not a map.
#[derive(Clone, Debug, PartialEq)]
pub struct Global {
    values: Arc<[f64]>,
}

impl Global {
    /// A global estimate of these values, or `ResourceLimit` when there are too many of them or one
    /// of them is not finite.
    pub fn new(values: impl Into<Arc<[f64]>>) -> Result<Self, Error> {
        let values = values.into();
        if values.len() > MAX_GLOBAL_VALUES {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "a spatial global estimate of {} values exceeds the {MAX_GLOBAL_BYTES} byte bound",
                    values.len()
                ),
            ));
        }
        if !values.iter().all(|value| value.is_finite()) {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                "a spatial global estimate holds a value that is not finite",
            ));
        }
        Ok(Self { values })
    }

    pub fn values(&self) -> &[f64] {
        &self.values
    }
}

/// One rectangle of planar `f32` linear-sRGB RGB a unit reads, with its position in the stage.
///
/// The three planes are contiguous: one complete R plane of `region.width × region.height` values
/// in row-major order, then G, then B.
#[derive(Clone, Copy, Debug)]
pub struct Planes<'a> {
    stage: Stage,
    region: Region,
    values: &'a [f32],
}

impl<'a> Planes<'a> {
    /// Borrow planar values as the rectangle `region` of `stage`. The length must be exactly three
    /// planes of that rectangle.
    pub fn new(stage: Stage, region: Region, values: &'a [f32]) -> Result<Self, Error> {
        check_planes(stage, region, values.len())?;
        Ok(Self {
            stage,
            region,
            values,
        })
    }

    /// The stage these values belong to. Reads outside the rectangle are clamped to it.
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The rectangle of the stage these values cover.
    pub fn region(&self) -> Region {
        self.region
    }

    /// The rectangle a unit of this halo must fill when it is given these planes as its input.
    /// It is [`Region::shrunk`] of [`Self::region`], and it is exactly the rectangle of the
    /// [`PlanesMut`] the host hands the unit alongside these.
    pub fn output_region(&self, halo: u32) -> Region {
        self.region.shrunk(halo, self.stage)
    }

    /// One pixel of these planes, or `None` outside the rectangle they cover.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        if !self.region.contains(x, y) {
            return None;
        }
        let len = self.region.pixels() as usize;
        let index = ((u64::from(y - self.region.y0)) * u64::from(self.region.width)
            + u64::from(x - self.region.x0)) as usize;
        Some([
            self.values[index],
            self.values[len + index],
            self.values[2 * len + index],
        ])
    }

    /// One pixel with the stage's edge clamping: a coordinate outside the stage reads the nearest
    /// stage pixel, which is the rule the host promises. The host always gives a unit an input
    /// rectangle wide enough that a read within the unit's declared halo of its output rectangle,
    /// clamped to the stage this way, lies inside the rectangle; a read further than that panics
    /// rather than quietly returning a neighbour.
    pub fn sample(&self, x: i64, y: i64) -> [f32; 3] {
        let x = x.clamp(0, i64::from(self.stage.width.saturating_sub(1))) as u32;
        let y = y.clamp(0, i64::from(self.stage.height.saturating_sub(1))) as u32;
        self.pixel(x, y).unwrap_or_else(|| {
            panic!(
                "a spatial unit read ({x}, {y}), outside the {:?} it was given",
                self.region
            )
        })
    }

    /// One plane of these values: `0` is red, `1` green, `2` blue.
    pub fn plane(&self, channel: usize) -> &'a [f32] {
        let len = self.region.pixels() as usize;
        &self.values[channel * len..(channel + 1) * len]
    }
}

/// The rectangle of planar `f32` linear-sRGB RGB a unit writes, with its position in the stage.
/// Its layout is [`Planes`]'s.
#[derive(Debug)]
pub struct PlanesMut<'a> {
    stage: Stage,
    region: Region,
    values: &'a mut [f32],
}

impl<'a> PlanesMut<'a> {
    /// Borrow planar values as the rectangle `region` of `stage`.
    pub fn new(stage: Stage, region: Region, values: &'a mut [f32]) -> Result<Self, Error> {
        check_planes(stage, region, values.len())?;
        Ok(Self {
            stage,
            region,
            values,
        })
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The rectangle this unit must fill completely: every pixel of it is read afterwards, either
    /// by the next unit or by the host's output boundary.
    pub fn region(&self) -> Region {
        self.region
    }

    /// Write one pixel. A coordinate outside the rectangle panics: the host sized the rectangle
    /// from the unit's own declared halo, so a write outside it is a unit that disagrees with what
    /// it declared.
    pub fn set(&mut self, x: u32, y: u32, rgb: [f32; 3]) {
        assert!(
            self.region.contains(x, y),
            "a spatial unit wrote ({x}, {y}), outside the {:?} it must fill",
            self.region
        );
        let len = self.region.pixels() as usize;
        let index = ((u64::from(y - self.region.y0)) * u64::from(self.region.width)
            + u64::from(x - self.region.x0)) as usize;
        self.values[index] = rgb[0];
        self.values[len + index] = rgb[1];
        self.values[2 * len + index] = rgb[2];
    }

    /// One plane to write in bulk: `0` is red, `1` green, `2` blue, row-major over the rectangle.
    pub fn plane_mut(&mut self, channel: usize) -> &mut [f32] {
        let len = self.region.pixels() as usize;
        &mut self.values[channel * len..(channel + 1) * len]
    }
}

fn check_planes(stage: Stage, region: Region, len: usize) -> Result<(), Error> {
    if region.x1() > stage.width || region.y1() > stage.height {
        return Err(Error::new(
            ErrorKind::Internal,
            format!(
                "a spatial rectangle {region:?} lies outside its {}x{} stage",
                stage.width, stage.height
            ),
        ));
    }
    if len as u64 != region.pixels() * 3 {
        return Err(Error::new(
            ErrorKind::Internal,
            format!(
                "a spatial rectangle {region:?} needs {} planar values, not {len}",
                region.pixels() * 3
            ),
        ));
    }
    Ok(())
}

/// One neighbourhood-dependent step, owned by the module that compiled it.
///
/// The host evaluates a whole operation over one output tile at a time, so a unit only ever sees a
/// rectangle of its stage and must produce, for every pixel it writes, the value a whole-frame
/// evaluation would produce there. Everything below is what the host guarantees and what it
/// requires in return.
///
/// **The rectangles.** [`Self::apply`] receives an input rectangle and an output rectangle. The
/// output rectangle is always `input.region().shrunk(halo, stage)`, available as
/// [`Planes::output_region`]: the input shrunk by the unit's own declared halo on every side except
/// a side that lies on the stage edge, where the unit writes up to the edge. The unit must fill
/// every pixel of the output rectangle and may read any pixel within `halo` of it, using
/// [`Planes::sample`], which clamps to the stage exactly as the host's own read does. A unit never
/// sees a coordinate outside the stage, and it never needs to invent one.
///
/// **The chain.** For an output tile `T` and units with halos `h1..hn` in evaluation order, the
/// host reads the input region `T` grown by `h1 + ... + hn` and clamped to the stage, gives it to
/// unit 1, gives unit 1's output rectangle to unit 2, and so on; the last unit's output rectangle
/// contains `T`, and the host takes `T` out of it. Away from the stage edges that means unit 1
/// fills `T` grown by `h2 + ... + hn`, unit 2 fills `T` grown by `h3 + ... + hn` and the last unit
/// fills exactly `T`. Against a stage edge the rectangles stay wider, because the shrink rule above
/// does not shrink a side that has nothing beyond it; a unit that follows the rule is unaffected,
/// and the host only ever reads `T` out of the result. The intermediate planes are the host's
/// scratch, charged to its spatial budget.
///
/// **Tile invariance is required, not hoped for.** The host chooses the tiling, runs tiles
/// concurrently on the shared Rayon pool and evaluates a single tile again for a point sample, so
/// a unit's value at a pixel must depend only on that pixel's neighbourhood in the stage and on its
/// own coefficients. Evaluate a filter by direct summation in a fixed order rather than by a
/// running sum, or the value will depend on where evaluation started.
///
/// **Values.** Planes are linear sRGB (D65) and may hold values outside `[0, 1]`: the host clamps
/// and quantizes once, after the last unit, at the output boundary. Alpha is the host's and is
/// never passed in. A non-finite value after any unit fails the render or the sample with
/// `resource-limit`.
pub trait SpatialUnit: Send + Sync {
    /// How many input pixels beyond its output rectangle this unit reads at this stage. The host
    /// sums the halos of an operation's units and refuses the operation when the sum exceeds
    /// [`MAX_SPATIAL_HALO`], before any pixel is read.
    fn halo(&self, stage: Stage) -> u32;

    /// How many bytes of scratch this unit needs to process an input rectangle of these
    /// dimensions. The host allocates the largest request of the operation's units once per tile,
    /// charges it to the spatial budget and passes it to every unit, so a unit must treat the
    /// slice as uninitialized and must not expect its own values back on the next tile.
    fn scratch_bytes(&self, region: Stage) -> u64;

    /// The global estimate this unit wants, prepared once from a bounded reduction of the whole
    /// operation input stage, or `None` when the unit needs none. The host caches the answer,
    /// including `None`, keyed by the source, the layers before this operation, the stage and the
    /// unit's position in the operation.
    fn prepare(&self, reduction: &Reduction) -> Option<Global>;

    /// Fill `output.region()` from `input`, reading no further than [`Self::halo`] beyond it.
    fn apply(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        global: Option<&Global>,
        scratch: &mut [f32],
    ) -> Result<(), Error>;

    /// Whether this unit's own coefficients are finite. Compilation refuses a unit that says no, so
    /// a non-finite parameter fails before a frame is touched.
    fn is_finite(&self) -> bool;

    /// A short, stable description of this unit and its coefficients. The host compares compiled
    /// operations by it, so two units that describe themselves identically must process
    /// identically.
    fn describe(&self) -> String;
}

/// What one spatial-stage layer compiles into: an ordered, bounded chain of neighbourhood units the
/// host evaluates as one tiled pass at a stage boundary, with the mask the host modulates that pass
/// by.
///
/// The mask is the host's half of the primitive and a module never sets it: a module compiles its
/// payload into units and returns a plain operation, and [`SpatialOperation::with_mask`] attaches
/// the [`CompiledMask`] the layer's own `mask` reference names. **A mask changes nothing about the
/// neighbourhood** — the halo, the tiling, the scratch, the batch concurrency and the global
/// estimate are all what they were, and every unit still reads the finished frame before the
/// operation and writes the next one. What changes is the *write*: the host blends the chain's
/// output at the output rectangle against the same input that tile already holds, per channel, in
/// linear light, before quantization on the byte path, and copies a tile the mask cannot reach
/// without evaluating a unit at all (`docs/design/masking.md`, "Masked colour and masked spatial").
#[derive(Clone, Default)]
pub struct SpatialOperation {
    units: Vec<Arc<dyn SpatialUnit>>,
    mask: Option<Arc<CompiledMask>>,
}

impl SpatialOperation {
    /// An operation over these units in evaluation order, or `ResourceLimit` when there are more
    /// than [`MAX_SPATIAL_UNITS`] of them or one of them declares non-finite coefficients. The
    /// stage-dependent bounds — the summed halo and the per-tile working set — are the host's, and
    /// it checks them when it compiles the recipe against a stage.
    pub fn new(units: Vec<Arc<dyn SpatialUnit>>) -> Result<Self, Error> {
        let operation = Self { units, mask: None };
        operation.validate()?;
        Ok(operation)
    }

    /// The same operation modulated by one compiled mask. Host-only: the mask comes from the
    /// layer's `mask` reference, which no module parses, plans or compiles.
    pub(crate) fn with_mask(mut self, mask: Arc<CompiledMask>) -> Self {
        self.mask = Some(mask);
        self
    }

    /// The mask this operation is modulated by, or `None` for an operation that applies everywhere
    /// and therefore keeps today's exact tile path, byte for byte.
    pub(crate) fn mask(&self) -> Option<&Arc<CompiledMask>> {
        self.mask.as_ref()
    }

    /// The operation a neutral payload compiles to: no units, which the host drops entirely, so a
    /// neutral layer opens no stage boundary, keeps the identity byte path and shares the source
    /// buffer.
    pub fn neutral() -> Self {
        Self::default()
    }

    /// The checks [`Self::new`] makes, so the host can make them again on an operation it was
    /// handed rather than trusting the constructor a module used.
    pub fn validate(&self) -> Result<(), Error> {
        if self.units.len() > MAX_SPATIAL_UNITS {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "a spatial operation declares {} units, more than the {MAX_SPATIAL_UNITS} the host evaluates",
                    self.units.len()
                ),
            ));
        }
        if !self.is_finite() {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                "a spatial operation declares a unit whose coefficients are not finite",
            ));
        }
        Ok(())
    }

    pub fn units(&self) -> &[Arc<dyn SpatialUnit>] {
        &self.units
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Whether every unit reports finite coefficients.
    pub fn is_finite(&self) -> bool {
        self.units.iter().all(|unit| unit.is_finite())
    }

    /// The halos of this operation's units at this stage, in evaluation order.
    pub fn halos(&self, stage: Stage) -> Vec<u32> {
        self.units.iter().map(|unit| unit.halo(stage)).collect()
    }

    /// How far beyond an output tile the host must read at this stage: the units are sequential, so
    /// their halos add. Saturating, so a unit that declares an absurd halo is refused rather than
    /// wrapping to a small one.
    pub fn summed_halo(&self, stage: Stage) -> u32 {
        self.units
            .iter()
            .fold(0_u32, |total, unit| total.saturating_add(unit.halo(stage)))
    }

    /// A short, stable description of the whole chain.
    pub fn describe(&self) -> String {
        self.units
            .iter()
            .map(|unit| unit.describe())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Two operations are the same when their units describe themselves the same way in the same order
/// and they are modulated by the same compiled mask: a trait object carries no structural identity,
/// so the description is the comparison. A compiled mask is compared by allocation, exactly as
/// [`ColorOperation`](crate::modules::ColorOperation) compares its own — conservative, because the
/// coverage field has no cheaper identity and nothing in the host depends on the other answer.
impl PartialEq for SpatialOperation {
    fn eq(&self, other: &Self) -> bool {
        let masks = match (&self.mask, &other.mask) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        };
        masks
            && self.units.len() == other.units.len()
            && std::iter::zip(&self.units, &other.units)
                .all(|(left, right)| left.describe() == right.describe())
    }
}

impl std::fmt::Debug for SpatialOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut list = f.debug_list();
        list.entries(self.units.iter().map(|unit| unit.describe()));
        if let Some(mask) = &self.mask {
            list.entry(&format!(
                "masked by {} components over {}x{}",
                mask.components(),
                mask.stage().width,
                mask.stage().height
            ));
        }
        list.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE: Stage = Stage {
        width: 1000,
        height: 700,
    };

    fn region(x0: u32, y0: u32, width: u32, height: u32) -> Region {
        Region {
            x0,
            y0,
            width,
            height,
        }
    }

    #[test]
    fn growing_and_shrinking_a_region_are_inverse_away_from_the_stage_edges() {
        let tile = region(512, 256, 256, 128);
        let grown = tile.grown(40, STAGE);
        assert_eq!(grown, region(472, 216, 336, 208));
        assert_eq!(grown.shrunk(40, STAGE), tile);
    }

    #[test]
    fn a_region_on_the_stage_edge_keeps_that_side_when_it_shrinks() {
        // A tile at the origin: growing clamps at 0 and the shrink leaves that side alone, so the
        // unit fills up to the edge rather than leaving a strip nobody writes.
        let tile = region(0, 0, 512, 512);
        let grown = tile.grown(40, STAGE);
        assert_eq!(grown, region(0, 0, 552, 552));
        assert_eq!(grown.shrunk(40, STAGE), region(0, 0, 512, 512));
        // A tile at the far edge: the right and bottom sides clamp to the stage.
        let tile = region(512, 512, 488, 188);
        let grown = tile.grown(40, STAGE);
        assert_eq!(grown, region(472, 472, 528, 228));
        assert_eq!(grown.shrunk(40, STAGE), tile);
        // A whole small stage is one tile touching every edge, so nothing shrinks at all.
        let small = Stage {
            width: 30,
            height: 20,
        };
        let whole = region(0, 0, 30, 20);
        assert_eq!(whole.grown(9, small), whole);
        assert_eq!(whole.grown(9, small).shrunk(9, small), whole);
    }

    #[test]
    fn a_chain_of_shrinks_always_still_contains_its_tile() {
        let stage = Stage {
            width: 1200,
            height: 900,
        };
        for (x0, y0) in [(0, 0), (512, 0), (0, 512), (512, 512), (1024, 768)] {
            let tile = region(
                x0,
                y0,
                (stage.width - x0).min(SPATIAL_TILE),
                (stage.height - y0).min(SPATIAL_TILE),
            );
            let halos = [37_u32, 5, 60];
            let total: u32 = halos.iter().sum();
            let mut current = tile.grown(total, stage);
            for halo in halos {
                current = current.shrunk(halo, stage);
            }
            assert!(
                current.x0 <= tile.x0
                    && current.y0 <= tile.y0
                    && current.x1() >= tile.x1()
                    && current.y1() >= tile.y1(),
                "the last unit's rectangle {current:?} must contain the tile {tile:?}"
            );
        }
    }

    #[test]
    fn a_global_estimate_is_bounded_and_finite() {
        assert_eq!(MAX_GLOBAL_VALUES, 512);
        let values: Vec<f64> = (0..MAX_GLOBAL_VALUES).map(|value| value as f64).collect();
        assert!(Global::new(values.clone()).is_ok());
        let mut too_many = values.clone();
        too_many.push(0.0);
        let error = Global::new(too_many).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        let error = Global::new(vec![f64::NAN]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
    }

    #[test]
    fn planes_read_and_write_their_rectangle_in_stage_coordinates() {
        let stage = Stage {
            width: 8,
            height: 6,
        };
        let rect = region(2, 1, 4, 3);
        let values: Vec<f32> = (0..rect.pixels() as usize * 3)
            .map(|index| index as f32)
            .collect();
        let planes = Planes::new(stage, rect, &values).unwrap();
        assert_eq!(planes.pixel(2, 1), Some([0.0, 12.0, 24.0]));
        assert_eq!(planes.pixel(5, 3), Some([11.0, 23.0, 35.0]));
        assert_eq!(planes.pixel(1, 1), None, "outside the rectangle");
        // A read outside the stage clamps to the stage, which is the host's rule; the host only
        // ever hands a unit a rectangle that reaches the edge when the clamped read lands in it.
        let edge = region(0, 0, 3, 2);
        let values: Vec<f32> = (0..edge.pixels() as usize * 3)
            .map(|index| index as f32)
            .collect();
        let planes = Planes::new(stage, edge, &values).unwrap();
        assert_eq!(planes.sample(-4, 0), planes.pixel(0, 0).unwrap());
        assert_eq!(planes.sample(1, -9), planes.pixel(1, 0).unwrap());
        let mut written = vec![0.0; rect.pixels() as usize * 3];
        let mut output = PlanesMut::new(stage, rect, &mut written).unwrap();
        output.set(3, 2, [1.0, 2.0, 3.0]);
        assert_eq!(output.plane_mut(1)[rect.width as usize + 1], 2.0);
    }
}
