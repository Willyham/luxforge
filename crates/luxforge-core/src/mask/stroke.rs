//! The mask brush's own stroke type: what one stroke painted onto a brush component holds beside its
//! path, the radius range it takes and the colour it may be limited to.
//!
//! The grid, the decimation, the per-stroke bound and the content-addressed store are the host's
//! ([`crate::path`]); everything here is the brush's, declared as a [`StrokeKind`] so the store holds
//! it beside any other consumer's strokes, and the mask table declares where its references live
//! ([`StrokeCarrier`]).
use super::{DISTANCE_MIN, REFINE_MAX, REFINE_MIN};
use crate::{
    Error, Mask,
    path::{self, StrokeCarrier, StrokeId, StrokeKind, StrokeReference, StrokeType},
};
use serde::{Deserialize, Serialize};

/// The legal stroke radius, in normalized units where one unit is the content stage's height: the
/// **one** range every reader of a brush radius holds — the declared `size` `mask.add-stroke` takes,
/// [`Stroke::capture`] on the posted radius, the recheck of a stored stroke and the compile that
/// draws it — and the one each of them names when it refuses ([`size_range`]).
///
/// The floor is the smallest distance a frozen falloff divides by ([`DISTANCE_MIN`]), so no radius a
/// stroke can hold ever makes a divisor smaller than the study allows; stored on the grid it is two
/// steps. The ceiling covers the whole stage from any point on it, well inside the study's largest
/// distance.
pub(crate) const SIZE_MIN: f64 = DISTANCE_MIN;
pub(crate) const SIZE_MAX: f64 = 2.0;

// Every legal brush radius is one the host's decimation takes.
const _: () = assert!(SIZE_MIN > 0.0 && SIZE_MAX <= path::RADIUS_MAX);

/// Whether `size` is a legal stroke radius: finite and inside [`SIZE_MIN`]`..=`[`SIZE_MAX`].
pub(crate) fn size_is_legal(size: f64) -> bool {
    size.is_finite() && (SIZE_MIN..=SIZE_MAX).contains(&size)
}

/// The legal stroke radius as every refusal of one spells it.
pub(crate) fn size_range() -> String {
    format!("{SIZE_MIN}..={SIZE_MAX}")
}

/// One captured brush stroke: the decimated path, on the stored grid, with the settings it was
/// drawn with.
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
    /// Whole percentage units, for the same reason the positions and the size are integers: a
    /// stroke's identity is the hash of its bytes, and `serde_json` does not round-trip every
    /// `f64` — `0.026241222396492958` reads back one ulp away — so a stroke stored with a
    /// fractional setting could reparse to a different content address than the recipe references.
    /// The declared controls move in whole units, so nothing is lost by storing what they offer.
    feather: i32,
    flow: i32,
    erase: bool,
    /// The colour this stroke is limited to, when it carries one.
    ///
    /// It is part of the stroke and therefore part of what is hashed: a limited stroke and the same
    /// path drawn without a limit are two different strokes, exactly as an add and an erase of one
    /// path are. Omitted from the canonical bytes when there is none, so an unlimited stroke has the
    /// spelling it has always had and the same content address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colour: Option<ColourLimit>,
}

/// The colour a limited stroke was seeded on, and how tight the similarity around it is.
///
/// The seed is the pixel the operation the mask modulates receives where the stroke began, **as the
/// host samples it**: the three sRGB codes `crate::modules::StageContext::sample_before` answers,
/// decoded to linear light by the delivered decode when the stroke is compiled. It is **stored**, not
/// re-read, so the stroke reproduces its own limit from its bytes after any later edit and nothing is
/// sampled again when the picture is drawn. `refine` is the colour range's own slider and means what
/// it means there: a higher number is always a narrower hold.
///
/// **Both are stored as integers, and that is load-bearing rather than tidy.** A stroke is addressed
/// by the hash of its canonical bytes, so a value that does not survive a JSON round trip exactly
/// would give the reparsed stroke a different address from the one the recipe references — and
/// `serde_json` does not round-trip every `f64` (`0.026241222396492958` reads back one ulp away). The
/// positions and the radius are held as integer grid steps for the same reason; these two join them,
/// so the stored precision is a property of the type rather than of the code that happens to write
/// it. The codes are what the host can read at all, and the refine is quantized to the tenth its own
/// declared control moves in. Its bounds are the colour range's own declaration rather than restated:
/// it is the same slider on the same axis with the same meaning, so the editor holds one rule for
/// both.
///
/// The similarity itself is frozen in `docs/design/mask-study.md#the-colour-constraint` and is the
/// colour range's own falloff at one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColourLimit {
    /// The sRGB codes of the sampled pixel, in the domain of the operation the mask modulates.
    seed: [u8; 3],
    /// Tenths of a refine unit, which is the step the declared control moves in.
    refine: i32,
}

/// Refine is stored in tenths, so a stored limit carries `0..=1000` of them.
const REFINE_STEPS_PER_UNIT: f64 = 10.0;

impl ColourLimit {
    /// The limit for a pixel the host sampled, at this refine.
    ///
    /// The seed is bytes because that is what the host's own point sample answers; nothing is lost
    /// against what it could offer, and a code is exact where a decoded `f64` is not. The refine is
    /// rounded to the tenth its declared control moves in, and refused by name outside its range.
    pub fn sampled(seed: [u8; 3], refine: f64) -> Result<Self, Error> {
        if !refine.is_finite() || !(REFINE_MIN..=REFINE_MAX).contains(&refine) {
            return Err(Error::validation(format!(
                "stroke colour refine must be a number within {REFINE_MIN:.0}..={REFINE_MAX:.0}"
            )));
        }
        Ok(Self {
            seed,
            refine: (refine * REFINE_STEPS_PER_UNIT).round() as i32,
        })
    }

    /// The seed in **linear sRGB**, through the delivered decode, which is the domain the frozen
    /// similarity is evaluated in.
    pub fn seed(&self) -> [f64; 3] {
        let linear = crate::colour::srgb::decode_pixel(self.seed);
        [
            f64::from(linear[0]),
            f64::from(linear[1]),
            f64::from(linear[2]),
        ]
    }

    /// The sampled codes as stored, for a client that wants to show the swatch it holds.
    pub(crate) fn codes(&self) -> [u8; 3] {
        self.seed
    }

    /// The refine on its own `0..=100` axis.
    pub fn refine(&self) -> f64 {
        f64::from(self.refine) / REFINE_STEPS_PER_UNIT
    }
}

/// Equality on the stored values. A stroke therefore stays [`Eq`] and so does everything that
/// carries one, and asking whether two strokes are the same asks whether they hold the same numbers
/// — which is what the content address already answers.
impl PartialEq for Stroke {
    fn eq(&self, other: &Self) -> bool {
        self.points == other.points
            && self.size == other.size
            && self.feather == other.feather
            && self.flow == other.flow
            && self.erase == other.erase
            && self.colour == other.colour
    }
}

impl Eq for Stroke {}

impl Stroke {
    /// Capture a stroke from a path a client posted.
    ///
    /// The brush's own settings are checked first, by name — the radius in [`size_range`], feather
    /// and flow in `0..=100` — and the path is then captured onto the host's grid at the tolerance
    /// its radius takes ([`path::capture_grid`]), so the same posted path at the same size always
    /// produces the same stored positions and therefore the same [`StrokeId`].
    pub fn capture(
        points: &[[f64; 2]],
        size: f64,
        feather: f64,
        flow: f64,
        erase: bool,
    ) -> Result<Self, Error> {
        if !size_is_legal(size) {
            return Err(Error::validation(format!(
                "stroke size must be a number within {}",
                size_range()
            )));
        }
        for (field, value, min, max) in
            [("feather", feather, 0.0, 100.0), ("flow", flow, 0.0, 100.0)]
        {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(Error::validation(format!(
                    "stroke {field} must be a number within {min}..={max}"
                )));
            }
        }
        let size = path::quantize(size);
        Ok(Self {
            points: path::capture_grid(points, size)?,
            size,
            feather: feather.round() as i32,
            flow: flow.round() as i32,
            erase,
            colour: None,
        })
    }

    /// Limit this stroke to a colour: the seed the host sampled where the stroke began, and the
    /// refine that says how tight the hold is.
    ///
    /// It is a second step rather than a sixth argument to [`Self::capture`] because a limit is
    /// something a stroke *may* carry and the seed is not the client's to supply: the host reads the
    /// pixel the masked operation receives and calls this, so no request can name a colour the
    /// picture does not have at the position the stroke started from. Every bound a limit has is
    /// checked where it is built, by [`ColourLimit::sampled`], and a code needs no bound at all.
    pub fn with_colour_limit(mut self, limit: ColourLimit) -> Self {
        self.colour = Some(limit);
        self
    }

    /// The colour this stroke is limited to, when it carries a limit.
    pub fn colour_limit(&self) -> Option<ColourLimit> {
        self.colour
    }

    /// The stored positions in normalized coordinates, in drawn order.
    pub fn points(&self) -> impl Iterator<Item = [f64; 2]> + '_ {
        path::from_grid(&self.points)
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// The radius in normalized units.
    pub fn size(&self) -> f64 {
        path::steps_to_units(self.size)
    }

    pub fn feather(&self) -> f64 {
        f64::from(self.feather)
    }

    pub fn flow(&self) -> f64 {
        f64::from(self.flow)
    }

    pub fn erase(&self) -> bool {
        self.erase
    }

    /// The bytes this stroke is addressed and stored by ([`StrokeKind::canonical`]), callable
    /// without the trait in scope.
    pub fn canonical(&self) -> Vec<u8> {
        StrokeKind::canonical(self)
    }

    /// This stroke's content address ([`StrokeKind::id`]).
    pub fn id(&self) -> StrokeId {
        StrokeKind::id(self)
    }

    /// Parse stored bytes and verify they are the bytes `id` addresses
    /// ([`StrokeKind::from_stored`]).
    pub fn from_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error> {
        <Self as StrokeKind>::from_stored(id, stored)
    }

    /// The ranges [`Self::capture`] enforces, re-checked on the way back in, so bytes that hash
    /// correctly but hold an illegal number are still refused rather than evaluated.
    fn check_stored(&self, id: &StrokeId) -> Result<(), Error> {
        let bad = |what: &str| {
            Err(Error::incompatible(format!(
                "stored stroke {id} has an invalid {what}"
            )))
        };
        if let Some(what) = path::stored_path_fault(&self.points) {
            return bad(what);
        }
        if !size_is_legal(self.size()) {
            return Err(Error::incompatible(format!(
                "stored stroke {id} has an invalid size; a stroke size is within {}",
                size_range()
            )));
        }
        if !(0..=100).contains(&self.feather) {
            return bad("feather");
        }
        if !(0..=100).contains(&self.flow) {
            return bad("flow");
        }
        Ok(())
    }
}

/// The mask brush's strokes in the host's store.
impl StrokeKind for Stroke {
    const KIND: &'static str = "mask brush";

    /// Its compact JSON in declared field order, with every position an integer, so one stroke has
    /// one spelling and a reparse of the stored bytes hashes back to the same address.
    fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a stroke is serializable")
    }

    fn parse_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error> {
        let stroke: Self = serde_json::from_slice(stored).map_err(|error| {
            Error::incompatible(format!("stored stroke {id} is malformed: {error}"))
        })?;
        stroke.check_stored(id)?;
        Ok(stroke)
    }
}

/// A mask carries its strokes in its components' payloads, under the host's reserved
/// [`path::STROKES_FIELD`], and every one is a brush [`Stroke`]. The host reads exactly that field
/// of each component payload and parses nothing else, because a payload belongs to the component
/// kind that provides it.
impl StrokeCarrier for Mask {
    fn stroke_references(&self, found: &mut Vec<StrokeReference>) -> Result<(), Error> {
        for component in &self.components {
            let what = format!("component {} of mask {}", component.name, self.name);
            for id in path::references(&component.payload, &what)? {
                found.push(StrokeReference {
                    what: what.clone(),
                    id,
                    kind: StrokeType::of::<Stroke>(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
