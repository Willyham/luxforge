//! The host's path primitives: the coordinate grid a drawn path is stored on, the decimation that
//! puts it there, and the content-addressed store the strokes of every painting consumer live in.
//!
//! These are host primitives and nothing here is about any one feature that paints. A path is an
//! ordered list of positions in the content stage's normalized coordinates; a stroke is such a path
//! with whatever else its consumer draws it with, in a type the consumer declares ([`StrokeKind`]);
//! a [`StrokeId`] is the hash of a stroke's canonical bytes. Any action that takes a drawn path
//! declares a [`crate::ParameterKind::Points`] parameter, captures it onto the grid here
//! ([`capture_grid`]) and stores its strokes in the one [`StrokeTable`], and the recipe finds a
//! consumer's references through the [`StrokeCarrier`] that consumer declares. The mask brush is
//! the one consumer today (`crate::mask::Stroke`): its radius range, its feather and flow and its
//! colour limit are its own, declared beside the brush and not here.
//!
//! **Why a store at all.** Every history entry stores a complete recipe rather than a delta
//! ([history](../../../docs/specs/edit-history.md), [rule 10](../../../docs/engineering/performance-rules.md#rules)),
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
use crate::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    any::{Any, TypeId},
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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
/// ([limits](../../../docs/design/architecture.md#rendering-and-limits)) and a step is therefore one
/// pixel of the largest stage a path can be drawn on. Storing more digits than that would store
/// noise and, since every history entry carries the reference list, would store it repeatedly.
pub const COORDINATE_STEPS_PER_UNIT: f64 = 16384.0;

/// The legal range of a stored path coordinate: the frame, with one frame of overshoot on each
/// side, because a stroke that begins or ends off the canvas is an ordinary gesture. It is the same
/// range a drawn position takes anywhere else in the recipe.
pub(crate) const COORDINATE_MIN: f64 = -1.0;
pub(crate) const COORDINATE_MAX: f64 = 2.0;

const GRID_MIN: i32 = (COORDINATE_MIN * COORDINATE_STEPS_PER_UNIT) as i32;
const GRID_MAX: i32 = (COORDINATE_MAX * COORDINATE_STEPS_PER_UNIT) as i32;

/// The largest radius a path can be decimated for: the whole stored coordinate range. A consumer's
/// own radius range sits inside it and is the consumer's to declare and to refuse; this bound is
/// only what keeps a radius a number the grid can hold.
pub(crate) const RADIUS_MAX: f64 = COORDINATE_MAX - COORDINATE_MIN;

/// The decimation tolerance as a fraction of the stroke's own radius: a captured position dropped by
/// [`decimate`] is at most this share of the radius from the polyline that is kept.
///
/// Relative, because what a kept position costs grows with the radius. A brush component's grid
/// index takes its cell side from the largest radius and records a segment in every cell its
/// radius-grown box reaches, so one cell counts every segment within about one and a half radii of
/// it: at a fixed tolerance a large brush keeps every whole-pixel position a pointer posts and lists
/// hundreds of them in one cell. Four per cent is sized from the scrub measurement in
/// `docs/design/masking.md#the-occupancy-cap`: at two per cent a six-pass scrub still puts 66
/// segments in one cell at radius 0.05, at three the worst measured cell holds 55 and at four 47, so
/// four is the round share that keeps every measured workload at the default and smaller sizes under
/// the cap with a margin. The stored path then stays within a twenty-fifth of the radius of the
/// drawn one.
pub(crate) const DECIMATION_TOLERANCE_OF_RADIUS: f64 = 0.04;

/// The least decimation tolerance, in grid steps, whatever the radius: two steps, within 2 px of the
/// captured path on a 16384 px stage and half a pixel on a 4096 px one, below what a person can see.
/// A radius under fifty steps takes this floor rather than a smaller share of itself, so a small
/// brush never stores more positions than two steps keep.
pub(crate) const DECIMATION_TOLERANCE_MIN_STEPS: f64 = 2.0;

/// The least decimation tolerance in normalized units, which is what a client posting a path reads.
pub(crate) const DECIMATION_TOLERANCE_MIN: f64 =
    DECIMATION_TOLERANCE_MIN_STEPS / COORDINATE_STEPS_PER_UNIT;

/// The half diagonal of one stored grid cell, in normalized units: how far snapping moves a position
/// on its own.
///
/// It is `sqrt(2)/2` of a step and not half a step, because the two coordinates are rounded
/// independently: a position in the far corner of its cell moves by the cell's half diagonal, not by
/// half a step. The measured worst case over randomized captured paths sits just under the tolerance
/// plus this, which is what caught the half-step spelling.
pub(crate) const GRID_ROUNDING: f64 = std::f64::consts::FRAC_1_SQRT_2 / COORDINATE_STEPS_PER_UNIT;

/// The decimation tolerance of a stroke whose radius is `size_steps` grid steps as stored: the
/// relative share of that radius, never less than the floor. Reading the stored radius rather than
/// the posted one is what makes a desktop that decimates before it posts and a host that decimates
/// what it was posted use one number.
fn tolerance_steps(size_steps: i32) -> f64 {
    (DECIMATION_TOLERANCE_OF_RADIUS * f64::from(size_steps)).max(DECIMATION_TOLERANCE_MIN_STEPS)
}

/// The decimation tolerance of a stroke of radius `size`, in normalized units.
#[cfg(test)]
pub(crate) fn decimation_tolerance(size: f64) -> f64 {
    tolerance_steps(quantize(size)) / COORDINATE_STEPS_PER_UNIT
}

/// The whole deviation a captured position may end up at from the stored path of a stroke of radius
/// `size`, in normalized units, and the bound a stored path is checked against: the decimation
/// tolerance plus [`GRID_ROUNDING`].
#[cfg(test)]
pub(crate) fn stored_deviation(size: f64) -> f64 {
    decimation_tolerance(size) + GRID_ROUNDING
}

/// Positions one stroke may hold **after decimation**: the bound on the stored stroke of every
/// consumer. A stroke longer than this is refused by name rather than stored ([`capture_grid`]): at
/// a tolerance of at least two grid steps, 1024 positions describe a path far longer than any single
/// drag across a frame.
pub const POINTS_PER_STROKE: usize = 1024;

/// Positions one posted path may hold **before decimation**: the bound a `points` parameter
/// declares, checked on the raw path a client sends, which [`capture_grid`] then decimates and
/// holds to [`POINTS_PER_STROKE`].
///
/// It is a separate number on purpose. A path is posted raw — an agent need not decimate — so the
/// stored bound applied before decimation would refuse a long drag that decimates to a handful of
/// positions, and a posted bound applied after it would bound nothing. Sixteen times the stored
/// bound is a minute of continuous pointer at 240 Hz, which no gesture reaches, and it still keeps
/// one request under the transport's one-megabyte line at any coordinate spelling and one
/// decimation pass bounded.
pub(crate) const POSTED_POINTS_PER_STROKE: usize = 16 * POINTS_PER_STROKE;

/// Whether `radius` is one a path can be decimated for: finite, positive and no larger than
/// [`RADIUS_MAX`]. A consumer holds its strokes to its own, narrower range before it captures.
fn radius_is_decimable(radius: f64) -> bool {
    radius.is_finite() && radius > 0.0 && radius <= RADIUS_MAX
}

/// The refusal of a radius no path can be decimated for.
fn illegal_radius() -> Error {
    Error::validation(format!(
        "path radius must be a positive number no greater than {RADIUS_MAX}"
    ))
}

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
    pub(crate) fn of(canonical: &[u8]) -> Self {
        #[cfg(test)]
        crate::editor::read_counts::hashed();
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
            Err(Error::validation(
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

/// A stroke type a painting consumer declares: the payload one captured stroke holds beside its
/// grid path, the bytes it is addressed and stored by, and the ranges its stored bytes are checked
/// against.
///
/// The host owns the grid, the decimation, the per-stroke bound and the store; the consumer owns
/// everything else about its strokes — which settings they carry, what range a radius takes, what a
/// limit means — and none of it is named here. Every stored value is an integer or a byte for the
/// same reason the grid is: a stroke is addressed by the hash of its canonical bytes, and a value
/// that does not survive a JSON round trip exactly would give the reparsed stroke a different
/// address from the one the recipe references.
pub trait StrokeKind: std::fmt::Debug + Send + Sync + Sized + 'static {
    /// What a refusal calls a stroke of this type, e.g. `mask brush`.
    const NAME: &'static str;

    /// The bytes this stroke is addressed and stored by: one spelling per stroke, so a reparse of
    /// the stored bytes hashes back to the same address.
    fn canonical(&self) -> Vec<u8>;

    /// Parse stored bytes whose address [`Self::from_stored`] has already checked, and hold them to
    /// the consumer's own ranges. Every refusal is [`crate::ErrorKind::Incompatible`] and names `id`,
    /// because it is the store disagreeing with itself and not a request that could be corrected.
    fn parse_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error>;

    /// This stroke's content address.
    fn id(&self) -> StrokeId {
        StrokeId::of(&self.canonical())
    }

    /// Parse stored bytes and verify they are the bytes `id` addresses: the address first, so bytes
    /// that are not the ones named are never parsed at all.
    fn from_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error> {
        if &StrokeId::of(stored) != id {
            return Err(Error::incompatible(format!(
                "stored stroke {id} does not match its content address"
            )));
        }
        #[cfg(test)]
        crate::editor::read_counts::decoded();
        Self::parse_stored(id, stored)
    }
}

/// A stroke as the store holds it, whatever its consumer: the one thing the store needs of every
/// type — the bytes it writes — and the type it was declared as, so a consumer reads back only its
/// own strokes. Every [`StrokeKind`] is one.
pub trait StoredStroke: Any + std::fmt::Debug + Send + Sync {
    /// The declared type's name, as a refusal spells it.
    fn stroke_kind(&self) -> &'static str;
    /// The canonical bytes, which are what the catalog stores under the stroke's address.
    fn stored_bytes(&self) -> Vec<u8>;
}

impl<S: StrokeKind> StoredStroke for S {
    fn stroke_kind(&self) -> &'static str {
        S::NAME
    }

    fn stored_bytes(&self) -> Vec<u8> {
        self.canonical()
    }
}

/// A declared stroke type as a value: what a [`StrokeReference`] names so the store can read the
/// bytes at that address back as the consumer's own type.
#[derive(Clone, Copy)]
pub struct StrokeType {
    kind: &'static str,
    type_id: TypeId,
    decode: Decode,
}

/// Reads stored bytes back as one declared stroke type.
type Decode = fn(&StrokeId, &[u8]) -> Result<Arc<dyn StoredStroke>, Error>;

impl StrokeType {
    /// The declaration of stroke type `S`.
    pub fn of<S: StrokeKind>() -> Self {
        fn decode<S: StrokeKind>(
            id: &StrokeId,
            stored: &[u8],
        ) -> Result<Arc<dyn StoredStroke>, Error> {
            S::from_stored(id, stored).map(|stroke| Arc::new(stroke) as Arc<dyn StoredStroke>)
        }
        Self {
            kind: S::NAME,
            type_id: TypeId::of::<S>(),
            decode: decode::<S>,
        }
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }
}

impl std::fmt::Debug for StrokeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StrokeType").field(&self.kind).finish()
    }
}

impl PartialEq for StrokeType {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
    }
}

impl Eq for StrokeType {}

/// One stroke reference a recipe carries: the address, the stroke type its consumer declared for
/// it, and the object that carries it, named for a refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrokeReference {
    pub what: String,
    pub id: StrokeId,
    pub kind: StrokeType,
}

/// Where one consumer keeps its stroke references in a recipe, and which stroke type each names.
///
/// A payload belongs to its provider, so the host does not go looking for strokes: each part of the
/// recipe that carries them declares its references here, and [`crate::Recipe::stroke_references`]
/// is the list of those parts. The mask table is the one implementor, through each mask's brush
/// components.
pub(crate) trait StrokeCarrier {
    /// Append this part's references, in stored order.
    fn stroke_references(&self, found: &mut Vec<StrokeReference>) -> Result<(), Error>;
}

/// Capture a posted path for a stroke of radius `radius_steps` grid steps, as every consumer
/// captures one: snapped to the stored grid and decimated there at the tolerance that radius takes,
/// then held to [`POINTS_PER_STROKE`]. So the same posted path at the same radius is always the same
/// stored positions, and — with the consumer's own settings, which it checks before it calls this —
/// the same [`StrokeId`]. Every coordinate is checked finite and in range by name.
pub(crate) fn capture_grid(points: &[[f64; 2]], radius_steps: i32) -> Result<Vec<[i32; 2]>, Error> {
    let grid = decimate_to_grid(points, radius_steps)?;
    if grid.len() > POINTS_PER_STROKE {
        return Err(Error::resource_limit(format!(
            "stroke has {} positions after decimation; the limit is {POINTS_PER_STROKE} \
                 points per stroke",
            grid.len()
        )));
    }
    Ok(grid)
}

/// What is wrong with a stored grid path, if anything: the checks [`capture_grid`] makes, re-made on
/// the way back in, so bytes that hash correctly but hold an illegal path are refused rather than
/// evaluated. `None` for a legal one; otherwise the word a consumer's refusal names.
pub(crate) fn stored_path_fault(points: &[[i32; 2]]) -> Option<&'static str> {
    if points.is_empty() || points.len() > POINTS_PER_STROKE {
        return Some("point count");
    }
    if points.iter().any(|[x, y]| !in_grid(*x) || !in_grid(*y)) {
        return Some("position");
    }
    None
}

/// Stored grid positions in normalized coordinates, in drawn order.
pub(crate) fn from_grid(points: &[[i32; 2]]) -> impl Iterator<Item = [f64; 2]> + '_ {
    points
        .iter()
        .map(|[x, y]| [steps_to_units(*x), steps_to_units(*y)])
}

/// One stored grid length in normalized units.
pub(crate) fn steps_to_units(steps: i32) -> f64 {
    f64::from(steps) / COORDINATE_STEPS_PER_UNIT
}

fn in_grid(value: i32) -> bool {
    (GRID_MIN..=GRID_MAX).contains(&value)
}

/// Snap one normalized coordinate or length to the stored grid. Half away from zero, which is
/// `f64::round`, so the rule is one named function and not an expression that could be written two
/// ways.
pub(crate) fn quantize(value: f64) -> i32 {
    (value * COORDINATE_STEPS_PER_UNIT).round() as i32
}

/// Decimate a captured path of a stroke of radius `size` to the stored grid.
///
/// This is the whole decimation contract, and it is deterministic: snap every position to the grid,
/// drop the ones that repeat, then run Ramer–Douglas–Peucker on the grid at the stroke's own
/// tolerance, `DECIMATION_TOLERANCE_OF_RADIUS` of its radius as stored and never less than
/// `DECIMATION_TOLERANCE_MIN_STEPS`. Running on the grid rather than before it is what makes the
/// result stable — two captures that round to the same grid path decimate identically whatever
/// their last bits were — and it is why the bound a stored path keeps to the captured one is
/// `stored_deviation` rather than the tolerance alone.
///
/// The desktop decimates with its brush's size before it posts a stroke, through [`PathCapture`],
/// which is this function taken one position at a time, so the host receives a path that is already
/// on the grid and already short; re-running it on the posted path at the same size is idempotent
/// and is what makes a path posted by an agent that did not decimate arrive at the same stored bytes
/// as one drawn by hand. A radius no path can be decimated for — not finite, not positive, or past
/// [`RADIUS_MAX`] — is refused by name; the consumer's own radius range is the consumer's to hold.
pub fn decimate(points: &[[f64; 2]], size: f64) -> Result<Vec<[f64; 2]>, Error> {
    let mut capture = PathCapture::default();
    for point in points {
        capture.push(*point);
    }
    capture.decimated(size)
}

/// A path captured one position at a time, on the stored grid: [`decimate`] for a gesture that
/// draws its path as it goes.
///
/// A pushed position is checked and snapped once, when it arrives, and one that snaps into the cell
/// of the position before it is dropped there, so the capture holds exactly the grid path
/// [`decimate`] would snap the whole path to, and a pointer event costs the one position it adds.
/// [`Self::decimated`] then runs the one reduction over that held grid path, so it answers exactly
/// what [`decimate`] answers for every position pushed so far — the same function, not a copy of
/// it.
///
/// **The reduction is the one whole-path step, and it cannot be taken incrementally without changing
/// what is stored.** Its first split is the position farthest from the chord between the path's two
/// ends, and the far end moves with every position, so a new position can change which earlier ones
/// are kept. An incremental reduction would store a different path, and therefore a different
/// address and a different coverage, for the same gesture. It is `O(n log n)` integer arithmetic over
/// the held grid, with no snapping or checking repeated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathCapture {
    /// The positions pushed so far, snapped, with consecutive repeats dropped.
    grid: Vec<[i32; 2]>,
    /// How many positions were pushed, repeats included: the index the next one is named by.
    pushed: usize,
    /// The first position outside the stored range, by its index and axis. A path holding one is
    /// refused whole, as [`decimate`] refuses it, and nothing pushed after it is held.
    refused: Option<(usize, &'static str)>,
}

impl PathCapture {
    /// Take the next position of the path.
    pub fn push(&mut self, point: [f64; 2]) {
        let index = self.pushed;
        self.pushed += 1;
        if self.refused.is_some() {
            return;
        }
        match snap(point) {
            Ok(point) => {
                if self.grid.last() != Some(&point) {
                    self.grid.push(point);
                }
            }
            Err(axis) => self.refused = Some((index, axis)),
        }
    }

    /// How many positions were pushed.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pushed
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.pushed == 0
    }

    /// The path pushed so far, decimated for a stroke of radius `size`: exactly [`decimate`] of
    /// every position pushed, refusals included.
    pub fn decimated(&self, size: f64) -> Result<Vec<[f64; 2]>, Error> {
        if !radius_is_decimable(size) {
            return Err(illegal_radius());
        }
        Ok(from_grid(&self.grid_decimated(quantize(size))?).collect())
    }

    fn grid_decimated(&self, size_steps: i32) -> Result<Vec<[i32; 2]>, Error> {
        if let Some((index, axis)) = self.refused {
            return Err(Error::validation(format!(
                "path position {index} {axis} must be a number within \
                     {COORDINATE_MIN:.0}..={COORDINATE_MAX:.0}"
            )));
        }
        if self.grid.is_empty() {
            return Err(Error::validation("a path must hold at least one position"));
        }
        Ok(reduce(&self.grid, tolerance_steps(size_steps)))
    }
}

/// One position on the stored grid, or the axis that is outside the stored range.
fn snap([x, y]: [f64; 2]) -> Result<[i32; 2], &'static str> {
    for (axis, value) in [("x", x), ("y", y)] {
        if !value.is_finite() || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&value) {
            return Err(axis);
        }
    }
    Ok([quantize(x), quantize(y)])
}

fn decimate_to_grid(points: &[[f64; 2]], size_steps: i32) -> Result<Vec<[i32; 2]>, Error> {
    let mut capture = PathCapture::default();
    for point in points {
        capture.push(*point);
    }
    capture.grid_decimated(size_steps)
}

/// Ramer–Douglas–Peucker on grid coordinates at `tolerance` grid steps, iterative so a long captured
/// path cannot overflow the stack, and comparing squared distances so nothing is decided by a square
/// root.
fn reduce(points: &[[i32; 2]], tolerance: f64) -> Vec<[i32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let tolerance2 = tolerance * tolerance;
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
pub(crate) enum StrokeFault {
    /// The store holds nothing at this address.
    Missing,
    /// The store holds bytes at this address that are not the bytes it names, or that are not a
    /// legal stroke of the type the reference declares.
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

/// The strokes a recipe's references resolved to, and the ones that did not, of every consumer's
/// declared stroke type.
///
/// It is attached to a recipe when the recipe is read out of its store and is never serialized with
/// it, which is what makes "no catalog ever holds embedded stroke points" a property of the types
/// rather than of the code that happens to write them. Resolution is one lookup per distinct
/// reference and no replay of anything. Each stroke is held as the type its reference declared
/// ([`StrokeType`]) and read back only as that type ([`Self::get`], [`Self::resolve`]), so two
/// consumers share one store without either reading the other's strokes.
///
/// The origin names where the recipe came from — a history entry, a draft — so a refusal can say
/// which stored thing is broken as well as which stroke.
///
/// One pointer wide, and `None` until something is put in it: every recipe carries this field and
/// almost none of them reference a stroke, so the table a recipe without a painted edit carries
/// costs eight bytes and no allocation. It is also what keeps the enums that carry a recipe from
/// growing a large variant.
///
/// **Shared, never copied.** A table does not change once its recipe has been read, so cloning a
/// recipe — which every plan, draft, preview and cached history entry does — shares the one table
/// instead of copying up to [`crate::MASKS_PER_RECIPE`] masks of `crate::POINTS_PER_MASK`
/// positions. The one writer, a stroke command adding the stroke it captured to the recipe it is
/// building, copies on write: the map of pointers once, and never a stroke's positions.
#[derive(Clone, Debug, Default)]
pub struct StrokeTable(Option<Arc<Resolved>>);

#[derive(Clone, Debug)]
struct Resolved {
    origin: String,
    strokes: BTreeMap<StrokeId, Arc<dyn StoredStroke>>,
    faults: BTreeMap<StrokeId, StrokeFault>,
    /// Ids of `strokes` already known to be durable in the catalog's content-addressed store: read
    /// from there ([`StrokeTable::load`]) or written there since ([`StrokeTable::mark_stored`]).
    /// A commit writes every referenced id **outside** this set, rather than trying to track which
    /// ones a stroke command captured fresh — the table is shared and copied by every plan, draft and
    /// cached entry, and a set that answered "known stored" is safe under all of them by
    /// construction: hydrating a recipe from the store marks everything it resolves, so a table that
    /// has ever been read out of the catalog carries only its own fresh strokes unmarked. It is the
    /// same for every consumer's strokes, because it is about addresses and not about types.
    stored: BTreeSet<StrokeId>,
}

impl StrokeTable {
    pub fn new(origin: impl Into<String>) -> Self {
        Self(Some(Arc::new(Resolved {
            origin: origin.into(),
            strokes: BTreeMap::new(),
            faults: BTreeMap::new(),
            stored: BTreeSet::new(),
        })))
    }

    pub(crate) fn origin(&self) -> &str {
        self.0.as_ref().map_or("", |held| held.origin.as_str())
    }

    /// The table to write into: this one when no other recipe shares it, and otherwise a copy of
    /// its map of pointers, so a write never changes a table another recipe holds.
    fn held(&mut self) -> &mut Resolved {
        Arc::make_mut(self.0.get_or_insert_with(|| {
            Arc::new(Resolved {
                origin: String::new(),
                strokes: BTreeMap::new(),
                faults: BTreeMap::new(),
                stored: BTreeSet::new(),
            })
        }))
    }

    /// Record a resolved stroke under its own address, **not** yet known to be in the catalog's
    /// store: a stroke a paint command just captured, or anything else that produced one fresh. The
    /// address is recomputed rather than trusted, so a table cannot hold a stroke under a name that
    /// is not its content's. A commit writes every such id until it is `Self::mark_stored` or the
    /// recipe is read back out of the catalog.
    pub fn insert<S: StrokeKind>(&mut self, stroke: S) -> StrokeId {
        let id = stroke.id();
        self.held().strokes.insert(id.clone(), Arc::new(stroke));
        id
    }

    /// Resolve one reference from the bytes the catalog's store holds at its address, or from their
    /// absence, as the type the reference declares.
    ///
    /// A stroke read here is durable by construction — it was just read from the store — so it is
    /// marked known stored, and the address is trusted rather than recomputed: the declared type's
    /// [`StrokeKind::from_stored`] has already hashed the stored bytes once to check them against it,
    /// and re-deriving the same address from the same bytes a second time here would hash every
    /// hydrated stroke twice for nothing. Absent bytes are [`StrokeFault::Missing`]; bytes that are
    /// not the ones named, or not a legal stroke of the declared type, are [`StrokeFault::Corrupt`].
    pub(crate) fn load(&mut self, id: StrokeId, kind: StrokeType, stored: Option<&[u8]>) {
        let Some(stored) = stored else {
            self.fault(id, StrokeFault::Missing);
            return;
        };
        match (kind.decode)(&id, stored) {
            Ok(stroke) => {
                let held = self.held();
                held.strokes.insert(id.clone(), stroke);
                held.stored.insert(id);
            }
            Err(_) => self.fault(id, StrokeFault::Corrupt),
        }
    }

    /// Whether `id` is resolved or faulted already, so one address is read from the store once
    /// however many references name it.
    pub(crate) fn knows(&self, id: &StrokeId) -> bool {
        self.0
            .as_ref()
            .is_some_and(|held| held.strokes.contains_key(id) || held.faults.contains_key(id))
    }

    /// Whether `id` is already known to be durable in the catalog's store, so a commit need not
    /// write it again.
    pub(crate) fn is_known_stored(&self, id: &StrokeId) -> bool {
        self.0.as_ref().is_some_and(|held| held.stored.contains(id))
    }

    /// Mark `id` as now durable in the catalog's store, once a caller has written it there. Lets a
    /// caller that keeps writing through one table — a measured session, for instance — model the
    /// same fact a fresh hydration would record on its own, without paying to re-read what it just
    /// wrote.
    ///
    /// Every production write path re-reads its recipe through [`Self::load`] before it plans the
    /// next command ([`crate::editor`]'s entry cache), so nothing there needs this; it exists for a
    /// harness that builds entries directly and has to say so itself.
    #[cfg(test)]
    pub(crate) fn mark_stored(&mut self, id: StrokeId) {
        self.held().stored.insert(id);
    }

    /// Record that a reference could not be resolved. The reason is kept so the refusal can say
    /// which of the two it was.
    pub(crate) fn fault(&mut self, id: StrokeId, fault: StrokeFault) {
        self.held().faults.insert(id, fault);
    }

    fn held_stroke(&self, id: &StrokeId) -> Option<&dyn StoredStroke> {
        self.0
            .as_ref()
            .and_then(|held| held.strokes.get(id))
            .map(Arc::as_ref)
    }

    /// The stroke at `id`, when the table holds one of type `S` there.
    pub fn get<S: StrokeKind>(&self, id: &StrokeId) -> Option<&S> {
        self.held_stroke(id)
            .and_then(|stroke| (stroke as &dyn Any).downcast_ref::<S>())
    }

    /// The bytes the catalog stores for `id`, whichever consumer's stroke it is.
    pub(crate) fn stored_bytes(&self, id: &StrokeId) -> Option<Vec<u8>> {
        self.held_stroke(id).map(StoredStroke::stored_bytes)
    }

    /// Every resolved stroke, of every type, by address.
    pub fn strokes(&self) -> impl Iterator<Item = (&StrokeId, &dyn StoredStroke)> {
        self.0
            .iter()
            .flat_map(|held| held.strokes.iter())
            .map(|(id, stroke)| (id, stroke.as_ref()))
    }

    /// Whether some reference was not in the store at all. That is the one fault a later write can
    /// repair, by storing the same content under the same address, so a resolution holding one is
    /// never kept as if it were final.
    pub(crate) fn has_missing(&self) -> bool {
        self.0.as_ref().is_some_and(|held| {
            held.faults
                .values()
                .any(|fault| *fault == StrokeFault::Missing)
        })
    }

    /// Whether two tables are one shared table rather than two copies.
    #[cfg(test)]
    pub(crate) fn shares(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(one), Some(two)) => Arc::ptr_eq(one, two),
            _ => false,
        }
    }

    /// Resolve one reference as stroke type `S`, or say why it cannot be. This is the refusal the
    /// whole store is judged by: it names the stroke and the thing that referenced it, it is
    /// `Incompatible` like every other payload the host retains and cannot evaluate, and there is no
    /// branch anywhere that returns an empty stroke instead. A stroke the table holds as another
    /// consumer's type is refused by both names rather than read as this one.
    pub(crate) fn resolve<S: StrokeKind>(&self, id: &StrokeId) -> Result<&S, Error> {
        self.check(id, TypeId::of::<S>(), S::NAME)?;
        Ok(self.get(id).expect("the check found a stroke of this type"))
    }

    /// [`Self::resolve`] for a reference whose declared type is a value rather than a type: whether
    /// the table holds a stroke of that type at its address, with the same refusal when it does not.
    pub(crate) fn check_reference(&self, reference: &StrokeReference) -> Result<(), Error> {
        self.check(&reference.id, reference.kind.type_id, reference.kind.kind)
    }

    fn check(&self, id: &StrokeId, type_id: TypeId, kind: &str) -> Result<(), Error> {
        let origin = match self.origin() {
            "" => "this recipe",
            origin => origin,
        };
        if let Some(stroke) = self.held_stroke(id) {
            return if (stroke as &dyn Any).type_id() == type_id {
                Ok(())
            } else {
                Err(Error::incompatible(format!(
                    "stroke {id} of {origin} is a {} stroke, not a {kind} stroke",
                    stroke.stroke_kind()
                )))
            };
        }
        let fault = self
            .0
            .as_ref()
            .and_then(|held| held.faults.get(id).copied())
            .unwrap_or(StrokeFault::Missing);
        Err(Error::incompatible(format!(
            "stroke {id} of {origin} {}",
            fault.detail()
        )))
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
        Error::validation(format!(
            "{what} has a malformed {STROKES_FIELD} field: expected a list of stroke addresses"
        ))
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
pub(crate) mod tests;
