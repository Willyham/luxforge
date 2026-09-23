//! The host's path primitives: the coordinate grid a drawn path is stored on, the decimation that
//! puts it there, the stroke a painting action captures, and the content-addressed store those
//! strokes live in.
//!
//! These are host primitives and nothing here is about any one feature that paints. A path is an
//! ordered list of positions in the content stage's normalized coordinates; a [`Stroke`] is such a
//! path with the brush settings it was drawn with; a [`StrokeId`] is the hash of a stroke's
//! canonical bytes. Any action that takes a drawn path declares a [`crate::ParameterKind::Points`]
//! parameter and stores its strokes here. Two designs already want exactly this — the component
//! that paints coverage and the repair operations that paint a region — so it is declared once,
//! beside the recipe, rather than twice inside the features that use it.
//!
//! **Why a store at all.** Every history entry stores a complete recipe rather than a delta
//! ([history](../../docs/specs/edit-history.md), [rule 10](../../docs/engineering/performance-rules.md#rules)),
//! and one stroke is one entry, so a payload that embedded its stroke list would copy every earlier
//! stroke of that list into every later entry. Storing each stroke once under its hash and
//! referencing it by that hash leaves the *shape* of the growth alone — an entry still holds one
//! reference per stroke, so the total is still quadratic in the stroke count — and shrinks its
//! constant from the serialized size of a stroke to the serialized size of a reference: 968 bytes
//! to 35 on the measured shape, a factor of about 28. It is not linear and this file claims nothing
//! of the sort; the hash-chain variant that would be linear is recorded in the design and is not
//! built.
//!
//! **What a reference costs to use.** An entry stays a full snapshot: resolving one takes one
//! lookup per referenced stroke and replays nothing, so previewing, undoing, redoing and restoring
//! are what they were, and the history graph, its branches and its named versions are untouched.
//!
//! **What a broken reference does.** A reference the store does not hold, or whose stored bytes do
//! not hash to the reference, is incompatible data exactly as an unavailable effect is: it is
//! retained byte for byte, it keeps reading, listing and undoing working, and every path that would
//! *draw* the recipe — rendering, sampling, and the export that will compile the same way — refuses
//! it by name. It is never resolved to an empty stroke, because an empty stroke is a picture that
//! silently lost part of an edit.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The reserved payload field a stored payload lists its stroke references in.
///
/// A payload is opaque to the host — it belongs to the effect or component kind that provides it —
/// so there is one reserved top-level field name through which the host can see the references a
/// payload carries without parsing it. Any payload the recipe holds — a layer's, or one belonging to
/// any other object it carries — whose object has a `strokes` field must have an array of
/// [`StrokeId`] strings there and nothing else.
pub const STROKES_FIELD: &str = "strokes";

/// Grid steps per normalized coordinate unit, where one unit is the content stage's height on both
/// axes.
///
/// This is the whole of the stored precision rule: a coordinate is stored as an integer number of
/// steps and no finer, because 16384 px is the largest side the editor admits
/// ([limits](../../docs/design/architecture.md#rendering-and-limits)) and a step is therefore one
/// pixel of the largest stage a path can be drawn on. Storing more digits than that would store
/// noise and, since every history entry carries the reference list, would store it repeatedly.
pub const COORDINATE_STEPS_PER_UNIT: f64 = 16384.0;

/// The legal range of a stored path coordinate: the frame, with one frame of overshoot on each
/// side, because a stroke that begins or ends off the canvas is an ordinary gesture. It is the same
/// range a drawn position takes anywhere else in the recipe.
pub const COORDINATE_MIN: f64 = -1.0;
pub const COORDINATE_MAX: f64 = 2.0;

const GRID_MIN: i32 = (COORDINATE_MIN * COORDINATE_STEPS_PER_UNIT) as i32;
const GRID_MAX: i32 = (COORDINATE_MAX * COORDINATE_STEPS_PER_UNIT) as i32;

/// The decimation tolerance, in grid steps: a captured position dropped by [`decimate`] is at most
/// this far from the polyline that is kept.
///
/// Two steps, so a decimated path is within 2 px of the captured one on a 16384 px stage and within
/// half a pixel on a 4096 px one — below what a person can see and far below what a brush of any
/// usable radius draws.
pub const DECIMATION_TOLERANCE_STEPS: f64 = 2.0;

/// The tolerance of [`decimate`] in normalized units, which is what a client posting a path reads.
pub const DECIMATION_TOLERANCE: f64 = DECIMATION_TOLERANCE_STEPS / COORDINATE_STEPS_PER_UNIT;

/// The whole deviation a captured position may end up at from the stored path, in normalized units,
/// and the bound a stored path is checked against: the decimation tolerance plus the half diagonal
/// of one grid cell.
///
/// The second term is `sqrt(2)/2` of a step and not half a step, because the two coordinates are
/// rounded independently: a position in the far corner of its cell moves by the cell's half
/// diagonal, not by half a step. The measured worst case over randomized captured paths sits just
/// under this and over `2.5` steps, which is what caught the half-step spelling.
pub const STORED_DEVIATION: f64 =
    (DECIMATION_TOLERANCE_STEPS + std::f64::consts::FRAC_1_SQRT_2) / COORDINATE_STEPS_PER_UNIT;

/// Positions one stroke may hold after decimation. A stroke longer than this is refused by name
/// rather than stored: at two grid steps of tolerance, 1024 positions describe a path far longer
/// than any single drag across a frame.
pub const POINTS_PER_STROKE: usize = 1024;

/// The lowest and highest legal stroke radius, in normalized units where one unit is the content
/// stage's height. A radius is stored on the same grid as a position, so the floor is one grid step
/// — the smallest radius that is still a radius at the largest admissible stage — and the ceiling
/// covers the whole stage from any point on it.
pub const SIZE_MIN: f64 = 1.0 / COORDINATE_STEPS_PER_UNIT;
pub const SIZE_MAX: f64 = 2.0;

/// One stored stroke's content address: the first 128 bits of the SHA-256 of its canonical bytes,
/// lowercase hex.
///
/// 128 bits is the width at which a collision is not a thing that happens, and 32 hex characters is
/// what makes a reference cost 35 bytes in an entry's JSON — the number the whole store exists to
/// achieve. It carries no prefix for the same reason: it is not an allocated identity but the name
/// the content already has, so two captures of the same path are the same stroke by construction.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrokeId(String);

impl StrokeId {
    /// The address of these canonical bytes.
    pub fn of(canonical: &[u8]) -> Self {
        let digest = Sha256::digest(canonical);
        let leading = u128::from_be_bytes(digest[..16].try_into().expect("SHA-256 is 32 bytes"));
        Self(format!("{leading:032x}"))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.len() == 32
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(Self(value))
        } else {
            Err(Error::new(
                ErrorKind::Validation,
                "invalid StrokeId: expected 32 lowercase hexadecimal characters",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for StrokeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for StrokeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for StrokeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// One captured stroke: the decimated path, on the stored grid, with the settings it was drawn
/// with.
///
/// `size` is the radius in normalized units, one unit being the content stage's height on both
/// axes, so a round brush is round at any aspect ratio. `feather` and `flow` are the editor's usual
/// 0..=100 percentages. `erase` marks a stroke that takes away rather than adds, which is a
/// property of the stroke and therefore part of what is hashed: an add and an erase of the same
/// path are two different strokes.
///
/// Positions are held as integer grid steps, which is what makes the stored precision a property of
/// the type rather than of the code that happens to write it, and what makes two captures of one
/// path byte-identical and therefore the same stored object.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    /// Integer grid steps, `[x, y]` each, in drawn order.
    points: Vec<[i32; 2]>,
    /// Serialized as an integer number of grid steps for the same reason the positions are.
    size: i32,
    feather: f64,
    flow: f64,
    erase: bool,
}

/// Equality on the stored values, with the two percentages compared by their bits, exactly as a
/// every other stored `f64` in the recipe is. A stroke therefore stays [`Eq`] and so does everything
/// that carries one, and
/// asking whether two strokes are the same asks whether they hold the same numbers — which is what
/// the content address already answers, and answers reflexively for every `f64` pattern.
impl PartialEq for Stroke {
    fn eq(&self, other: &Self) -> bool {
        self.points == other.points
            && self.size == other.size
            && self.feather.to_bits() == other.feather.to_bits()
            && self.flow.to_bits() == other.flow.to_bits()
            && self.erase == other.erase
    }
}

impl Eq for Stroke {}

impl Stroke {
    /// Capture a stroke from a path a client posted.
    ///
    /// The path is snapped to the stored grid and decimated there, so the same posted path always
    /// produces the same stored positions and therefore the same [`StrokeId`]. Everything checkable
    /// is checked by name: every coordinate finite and in range, every setting in range, and the
    /// decimated length within [`POINTS_PER_STROKE`].
    pub fn capture(
        points: &[[f64; 2]],
        size: f64,
        feather: f64,
        flow: f64,
        erase: bool,
    ) -> Result<Self, Error> {
        let grid = decimate_to_grid(points)?;
        if grid.len() > POINTS_PER_STROKE {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "stroke has {} positions after decimation; the limit is {POINTS_PER_STROKE} \
                     points per stroke",
                    grid.len()
                ),
            ));
        }
        for (field, value, min, max) in [
            ("size", size, SIZE_MIN, SIZE_MAX),
            ("feather", feather, 0.0, 100.0),
            ("flow", flow, 0.0, 100.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!("stroke {field} must be a number within {min}..={max}"),
                ));
            }
        }
        Ok(Self {
            points: grid,
            size: quantize(size),
            feather,
            flow,
            erase,
        })
    }

    /// The stored positions in normalized coordinates, in drawn order.
    pub fn points(&self) -> impl Iterator<Item = [f64; 2]> + '_ {
        self.points.iter().map(|[x, y]| {
            [
                f64::from(*x) / COORDINATE_STEPS_PER_UNIT,
                f64::from(*y) / COORDINATE_STEPS_PER_UNIT,
            ]
        })
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// The radius in normalized units.
    pub fn size(&self) -> f64 {
        f64::from(self.size) / COORDINATE_STEPS_PER_UNIT
    }

    pub fn feather(&self) -> f64 {
        self.feather
    }

    pub fn flow(&self) -> f64 {
        self.flow
    }

    pub fn erase(&self) -> bool {
        self.erase
    }

    /// The bytes this stroke is addressed and stored by: its compact JSON in declared field order,
    /// with every position an integer, so one stroke has one spelling and a reparse of the stored
    /// bytes hashes back to the same address.
    pub fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a stroke is serializable")
    }

    pub fn id(&self) -> StrokeId {
        StrokeId::of(&self.canonical())
    }

    /// Parse stored bytes and verify they are the bytes `id` addresses.
    ///
    /// A mismatch is [`ErrorKind::Incompatible`] and names the reference, because it is the store
    /// disagreeing with itself and not a request that could be corrected.
    pub fn from_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error> {
        if &StrokeId::of(stored) != id {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("stored stroke {id} does not match its content address"),
            ));
        }
        let stroke: Self = serde_json::from_slice(stored).map_err(|error| {
            Error::new(
                ErrorKind::Incompatible,
                format!("stored stroke {id} is malformed: {error}"),
            )
        })?;
        stroke.check_stored(id)?;
        Ok(stroke)
    }

    /// The ranges [`Self::capture`] enforces, re-checked on the way back in, so bytes that hash
    /// correctly but hold an illegal number are still refused rather than evaluated.
    fn check_stored(&self, id: &StrokeId) -> Result<(), Error> {
        let bad = |what: &str| {
            Err(Error::new(
                ErrorKind::Incompatible,
                format!("stored stroke {id} has an invalid {what}"),
            ))
        };
        if self.points.is_empty() || self.points.len() > POINTS_PER_STROKE {
            return bad("point count");
        }
        if self
            .points
            .iter()
            .any(|[x, y]| !in_grid(*x) || !in_grid(*y))
        {
            return bad("position");
        }
        if !in_grid(self.size)
            || self.size < 1
            || f64::from(self.size) > SIZE_MAX * COORDINATE_STEPS_PER_UNIT
        {
            return bad("size");
        }
        if !self.feather.is_finite() || !(0.0..=100.0).contains(&self.feather) {
            return bad("feather");
        }
        if !self.flow.is_finite() || !(0.0..=100.0).contains(&self.flow) {
            return bad("flow");
        }
        Ok(())
    }
}

fn in_grid(value: i32) -> bool {
    (GRID_MIN..=GRID_MAX).contains(&value)
}

/// Snap one normalized coordinate to the stored grid. Half away from zero, which is `f64::round`,
/// so the rule is one named function and not an expression that could be written two ways.
fn quantize(value: f64) -> i32 {
    (value * COORDINATE_STEPS_PER_UNIT).round() as i32
}

/// Decimate a captured path to the stored grid.
///
/// This is the whole decimation contract, and it is deterministic: snap every position to the grid,
/// drop the ones that repeat, then run Ramer–Douglas–Peucker on the grid at
/// [`DECIMATION_TOLERANCE_STEPS`]. Running on the grid rather than before it is what makes the
/// result stable — two captures that round to the same grid path decimate identically whatever
/// their last bits were — and it is why the bound a stored path keeps to the captured one is
/// [`STORED_DEVIATION`] rather than the tolerance alone.
///
/// The desktop calls this before it posts a stroke, so the host receives a path that is already on
/// the grid and already short; re-running it here on the posted path is idempotent and is what makes
/// a path posted by an agent that did not decimate arrive at the same stored bytes as one drawn by
/// hand.
pub fn decimate(points: &[[f64; 2]]) -> Result<Vec<[f64; 2]>, Error> {
    Ok(decimate_to_grid(points)?
        .into_iter()
        .map(|[x, y]| {
            [
                f64::from(x) / COORDINATE_STEPS_PER_UNIT,
                f64::from(y) / COORDINATE_STEPS_PER_UNIT,
            ]
        })
        .collect())
}

fn decimate_to_grid(points: &[[f64; 2]]) -> Result<Vec<[i32; 2]>, Error> {
    if points.is_empty() {
        return Err(Error::new(
            ErrorKind::Validation,
            "a path must hold at least one position",
        ));
    }
    let mut snapped: Vec<[i32; 2]> = Vec::with_capacity(points.len());
    for (index, [x, y]) in points.iter().enumerate() {
        for (axis, value) in [("x", *x), ("y", *y)] {
            if !value.is_finite() || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&value) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!(
                        "path position {index} {axis} must be a number within \
                         {COORDINATE_MIN:.0}..={COORDINATE_MAX:.0}"
                    ),
                ));
            }
        }
        let point = [quantize(*x), quantize(*y)];
        if snapped.last() != Some(&point) {
            snapped.push(point);
        }
    }
    Ok(reduce(&snapped))
}

/// Ramer–Douglas–Peucker on grid coordinates, iterative so a long captured path cannot overflow the
/// stack, and comparing squared distances so nothing is decided by a square root.
fn reduce(points: &[[i32; 2]]) -> Vec<[i32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let tolerance2 = DECIMATION_TOLERANCE_STEPS * DECIMATION_TOLERANCE_STEPS;
    let mut stack = vec![(0_usize, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        if last <= first + 1 {
            continue;
        }
        let [ax, ay] = [f64::from(points[first][0]), f64::from(points[first][1])];
        let [bx, by] = [f64::from(points[last][0]), f64::from(points[last][1])];
        let (dx, dy) = (bx - ax, by - ay);
        let length2 = dx * dx + dy * dy;
        let mut worst = 0.0_f64;
        let mut at = first;
        for (index, point) in points.iter().enumerate().take(last).skip(first + 1) {
            let (px, py) = (f64::from(point[0]), f64::from(point[1]));
            let (ex, ey) = (px - ax, py - ay);
            // The perpendicular distance, squared, kept as a ratio so the comparison against the
            // squared tolerance below needs no division either: `cross² / length²` against `t²` is
            // `cross²` against `t² · length²`. A degenerate segment falls back to the endpoint
            // distance, which is the same quantity with `length² = 1`.
            let distance2 = if length2 > 0.0 {
                let cross = dx * ey - dy * ex;
                cross * cross / length2
            } else {
                ex * ex + ey * ey
            };
            if distance2 > worst {
                worst = distance2;
                at = index;
            }
        }
        if worst > tolerance2 {
            keep[at] = true;
            stack.push((first, at));
            stack.push((at, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(*point))
        .collect()
}

/// Why a stroke reference could not be resolved. Both are incompatible data the host retains, never
/// a stroke to skip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeFault {
    /// The store holds nothing at this address.
    Missing,
    /// The store holds bytes at this address that are not the bytes it names, or that are not a
    /// legal stroke.
    Corrupt,
}

impl StrokeFault {
    fn detail(self) -> &'static str {
        match self {
            Self::Missing => "is not in the stroke store",
            Self::Corrupt => "does not match its stored content address",
        }
    }
}

/// The strokes a recipe's references resolved to, and the ones that did not.
///
/// It is attached to a recipe when the recipe is read out of its store and is never serialized with
/// it, which is what makes "no catalog ever holds embedded stroke points" a property of the types
/// rather than of the code that happens to write them. Resolution is one lookup per distinct
/// reference and no replay of anything.
///
/// The origin names where the recipe came from — a history entry, a draft — so a refusal can say
/// which stored thing is broken as well as which stroke.
///
/// One pointer wide, and `None` until something is put in it: every recipe carries this field and
/// almost none of them reference a stroke, so the table a recipe without a painted edit carries
/// costs eight bytes and no allocation. It is also what keeps the enums that carry a recipe from
/// growing a large variant.
#[derive(Clone, Debug, Default)]
pub struct StrokeTable(Option<Box<Resolved>>);

#[derive(Clone, Debug)]
struct Resolved {
    origin: String,
    strokes: BTreeMap<StrokeId, Stroke>,
    faults: BTreeMap<StrokeId, StrokeFault>,
}

impl StrokeTable {
    pub fn new(origin: impl Into<String>) -> Self {
        Self(Some(Box::new(Resolved {
            origin: origin.into(),
            strokes: BTreeMap::new(),
            faults: BTreeMap::new(),
        })))
    }

    pub fn origin(&self) -> &str {
        self.0.as_ref().map_or("", |held| held.origin.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(|held| held.strokes.is_empty() && held.faults.is_empty())
    }

    fn held(&mut self) -> &mut Resolved {
        self.0.get_or_insert_with(|| {
            Box::new(Resolved {
                origin: String::new(),
                strokes: BTreeMap::new(),
                faults: BTreeMap::new(),
            })
        })
    }

    /// Record a resolved stroke under its own address. The address is recomputed rather than
    /// trusted, so a table cannot hold a stroke under a name that is not its content's.
    pub fn insert(&mut self, stroke: Stroke) -> StrokeId {
        let id = stroke.id();
        self.held().strokes.insert(id.clone(), stroke);
        id
    }

    /// Record that a reference could not be resolved. The reason is kept so the refusal can say
    /// which of the two it was.
    pub fn fault(&mut self, id: StrokeId, fault: StrokeFault) {
        self.held().faults.insert(id, fault);
    }

    pub fn get(&self, id: &StrokeId) -> Option<&Stroke> {
        self.0.as_ref().and_then(|held| held.strokes.get(id))
    }

    pub fn strokes(&self) -> impl Iterator<Item = (&StrokeId, &Stroke)> {
        self.0.iter().flat_map(|held| held.strokes.iter())
    }

    /// Resolve one reference, or say why it cannot be. This is the refusal the whole store is
    /// judged by: it names the stroke and the thing that referenced it, it is `Incompatible` like
    /// every other payload the host retains and cannot evaluate, and there is no branch anywhere
    /// that returns an empty stroke instead.
    pub fn resolve(&self, id: &StrokeId) -> Result<&Stroke, Error> {
        if let Some(stroke) = self.get(id) {
            return Ok(stroke);
        }
        let fault = self
            .0
            .as_ref()
            .and_then(|held| held.faults.get(id).copied())
            .unwrap_or(StrokeFault::Missing);
        let origin = self.origin();
        Err(Error::new(
            ErrorKind::Incompatible,
            format!(
                "stroke {id} of {} {}",
                if origin.is_empty() {
                    "this recipe"
                } else {
                    origin
                },
                fault.detail()
            ),
        ))
    }
}

/// The stroke references one stored payload carries, in stored order.
///
/// A payload is the provider's, so the host reads exactly the one reserved field and nothing else:
/// a payload without [`STROKES_FIELD`] carries no strokes, and a payload with one carries an array
/// of addresses. Anything else there is malformed and is refused by name rather than ignored,
/// because a payload the host half-understands is the case that silently loses an edit.
pub fn references(payload: &Value, what: &str) -> Result<Vec<StrokeId>, Error> {
    let Some(field) = payload.get(STROKES_FIELD) else {
        return Ok(Vec::new());
    };
    let malformed = || {
        Error::new(
            ErrorKind::Validation,
            format!(
                "{what} has a malformed {STROKES_FIELD} field: expected a list of stroke addresses"
            ),
        )
    };
    let listed = field.as_array().ok_or_else(malformed)?;
    listed
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(malformed)
                .and_then(|text| StrokeId::parse(text).map_err(|_| malformed()))
        })
        .collect()
}

#[cfg(test)]
mod tests;
