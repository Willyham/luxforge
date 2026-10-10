//! The Corrections stroke: one Clone or Heal operation's path and radius in the content stage, in
//! the host's one content-addressed stroke store ([`crate::path`]), and the repair layer's
//! declaration of where its references live ([`crate::path::StrokeCarrier`]).
use super::payload::OperationKind;
use crate::{
    Error, Layer,
    path::{
        self, StrokeCarrier, StrokeId, StrokeKind, StrokeReference, StrokeType, capture_grid,
        quantize, steps_to_units, stored_path_fault,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// The least radius a correction takes, in content heights: about 2 px on a 4,000 px-high photo.
pub(crate) const RADIUS_MIN: f64 = 0.0005;
/// The largest radius a correction takes, in content heights: a quarter of the frame's height.
pub(crate) const RADIUS_MAX: f64 = 0.25;

/// A requested radius in content heights, checked against the correction's own range by name, as
/// the grid stores it.
fn checked_radius(radius: f64) -> Result<i32, Error> {
    if !radius.is_finite() || !(RADIUS_MIN..=RADIUS_MAX).contains(&radius) {
        return Err(Error::validation(format!(
            "a correction's radius must be a number within {RADIUS_MIN}..={RADIUS_MAX} content \
             heights"
        )));
    }
    Ok(quantize(radius))
}

/// The stored radius range, in grid steps: the declared range as the grid stores it.
fn radius_steps() -> std::ops::RangeInclusive<i32> {
    quantize(RADIUS_MIN)..=quantize(RADIUS_MAX)
}

/// One correction's stroke: its path on the stored grid, in drawn order, and its radius in grid
/// steps. Canonical compact JSON in that field order, so a reparse hashes back to its address.
/// Feather, opacity, kind and source live in the operation, so changing them writes no stroke;
/// moving or resizing writes a new one. A spot is a one-point path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CorrectionStroke {
    /// Shared, so a compiled repair table holds the stroke's positions without copying them.
    #[serde(with = "shared_points")]
    points: Arc<[[i32; 2]]>,
    radius: i32,
}

/// A stroke's positions serialize as the plain array they are.
mod shared_points {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub(super) fn serialize<S: Serializer>(
        points: &Arc<[[i32; 2]]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        points.as_ref().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<[[i32; 2]]>, D::Error> {
        Vec::<[i32; 2]>::deserialize(deserializer).map(Arc::from)
    }
}

impl CorrectionStroke {
    /// Capture a posted path at `radius` content heights: the radius checked against the
    /// correction's own range by name first, then the path captured onto the host's grid exactly
    /// as every consumer's is ([`capture_grid`]), held to its stored bound.
    pub(crate) fn capture(points: &[[f64; 2]], radius: f64) -> Result<Self, Error> {
        let radius = checked_radius(radius)?;
        Ok(Self {
            points: capture_grid(points, radius)?.into(),
            radius,
        })
    }

    /// The same stored positions at another radius, in content heights: a resize keeps the path.
    pub(crate) fn with_radius(&self, radius: f64) -> Result<Self, Error> {
        Ok(Self {
            points: self.points.clone(),
            radius: checked_radius(radius)?,
        })
    }

    /// The stored positions in grid steps, shared.
    pub(crate) fn grid(&self) -> Arc<[[i32; 2]]> {
        Arc::clone(&self.points)
    }

    /// The anchor: the first stored position (a spot's centre, a stroke's pin), in grid steps.
    pub(crate) fn anchor(&self) -> [i32; 2] {
        self.points[0]
    }

    /// The stored positions in normalized content coordinates.
    pub(crate) fn points(&self) -> impl Iterator<Item = [f64; 2]> + '_ {
        path::from_grid(&self.points)
    }

    /// The radius in grid steps.
    pub(crate) fn radius_steps(&self) -> i32 {
        self.radius
    }

    /// The radius in content heights.
    pub(crate) fn radius(&self) -> f64 {
        steps_to_units(self.radius)
    }
}

impl StrokeKind for CorrectionStroke {
    const NAME: &'static str = "correction";

    fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a correction stroke is serializable")
    }

    fn parse_stored(id: &StrokeId, stored: &[u8]) -> Result<Self, Error> {
        let stroke: Self = serde_json::from_slice(stored).map_err(|error| {
            Error::incompatible(format!("stored stroke {id} is malformed: {error}"))
        })?;
        if let Some(what) = stored_path_fault(&stroke.points) {
            return Err(Error::incompatible(format!(
                "stored stroke {id} has an invalid {what}"
            )));
        }
        if !radius_steps().contains(&stroke.radius) {
            return Err(Error::incompatible(format!(
                "stored stroke {id} has an invalid radius"
            )));
        }
        Ok(stroke)
    }
}

/// The repair layer as a part of the recipe that carries strokes: its payload's reserved
/// [`path::STROKES_FIELD`] lists the stroke of each Clone/Heal operation in operation order, every
/// one a [`CorrectionStroke`]. The host reads that field alone; each reference is named by the
/// operation it belongs to, read leniently from the payload's operations, so a refusal of a
/// missing or corrupt stroke says which correction it was (`operation correction-5 of
/// Corrections`). A payload whose operations cannot be read that way is named as the layer.
pub(crate) struct RepairCarrier<'a>(pub(crate) &'a Layer);

impl StrokeCarrier for RepairCarrier<'_> {
    fn stroke_references(&self, found: &mut Vec<StrokeReference>) -> Result<(), Error> {
        let layer = self.0;
        let listed = path::references(&layer.payload, "the Corrections layer")?;
        let names = operation_names(&layer.payload).filter(|names| names.len() == listed.len());
        for (index, id) in listed.into_iter().enumerate() {
            let what = match &names {
                Some(names) => format!("operation correction-{} of Corrections", names[index]),
                None => "the Corrections layer".to_owned(),
            };
            found.push(StrokeReference {
                what,
                id,
                kind: StrokeType::of::<CorrectionStroke>(),
            });
        }
        Ok(())
    }
}

/// The numbers of the payload's stroke-carrying operations, in order, when they can be read.
fn operation_names(payload: &Value) -> Option<Vec<u64>> {
    payload
        .get("operations")?
        .as_array()?
        .iter()
        .filter(|operation| {
            operation
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| OperationKind::parse(kind).is_some())
        })
        .map(|operation| operation.get("id").and_then(Value::as_u64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::StrokeTable;

    #[test]
    fn a_correction_stroke_captures_on_the_grid_and_round_trips_its_address() {
        let stroke = CorrectionStroke::capture(&[[0.25, 0.5], [0.3, 0.5], [0.35, 0.52]], 0.02)
            .expect("a legal stroke");
        assert_eq!(stroke.radius_steps(), quantize(0.02));
        assert_eq!(stroke.anchor(), [quantize(0.25), quantize(0.5)]);
        let bytes = stroke.canonical();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .starts_with("{\"points\":[[")
        );
        let id = stroke.id();
        assert_eq!(CorrectionStroke::from_stored(&id, &bytes).unwrap(), stroke);
        // A spot is one point.
        let spot = CorrectionStroke::capture(&[[0.5, 0.5]], 0.01).unwrap();
        assert_eq!(spot.points().count(), 1);
        // The same posted path at the same radius is the same stroke; another radius is another.
        assert_eq!(
            CorrectionStroke::capture(&[[0.5, 0.5]], 0.01).unwrap().id(),
            spot.id()
        );
        assert_ne!(spot.with_radius(0.02).unwrap().id(), spot.id());
        assert!(spot.with_radius(0.02).unwrap().points().eq(spot.points()));
    }

    #[test]
    fn a_correction_radius_outside_its_range_is_refused_by_name() {
        for radius in [0.0, 0.0004, 0.26, f64::NAN] {
            let error = CorrectionStroke::capture(&[[0.5, 0.5]], radius).unwrap_err();
            assert_eq!(error.kind, crate::ErrorKind::Validation, "{radius}");
            assert!(error.detail.contains("radius"), "{}", error.detail);
        }
        // Stored bytes are held to the same range on the way back in.
        let wide = br#"{"points":[[100,100]],"radius":9000}"#;
        let id = StrokeId::of(wide);
        let error = CorrectionStroke::from_stored(&id, wide).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Incompatible);
        assert!(error.detail.contains("radius"));
        let empty = br#"{"points":[],"radius":100}"#;
        assert!(
            CorrectionStroke::from_stored(&StrokeId::of(empty), empty)
                .unwrap_err()
                .detail
                .contains("point count")
        );
    }

    #[test]
    fn the_repair_carrier_names_each_reference_by_its_operation() {
        let one = CorrectionStroke::capture(&[[0.2, 0.2]], 0.01).unwrap();
        let two = CorrectionStroke::capture(&[[0.6, 0.6]], 0.01).unwrap();
        let layer = Layer::new(
            super::super::CORRECTIONS_EFFECT,
            serde_json::json!({
                "next": 8,
                "strokes": [one.id(), two.id()],
                "operations": [
                    {"id": 2, "kind": "heal", "stroke": one.id(), "feather": 50, "opacity": 100,
                     "offset": [100, 0], "origin": "auto"},
                    {"id": 7, "kind": "clone", "stroke": two.id(), "feather": 0, "opacity": 80,
                     "offset": [0, 100], "origin": "set"},
                ],
            }),
        );
        let mut found = Vec::new();
        RepairCarrier(&layer).stroke_references(&mut found).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].what, "operation correction-2 of Corrections");
        assert_eq!(found[1].what, "operation correction-7 of Corrections");
        assert_eq!(found[1].kind, StrokeType::of::<CorrectionStroke>());
        // A stroke the table does not hold is refused naming the address, never read as empty.
        let table = StrokeTable::new("entry e");
        let error = table.check_reference(&found[0]).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Incompatible);
        assert!(error.detail.contains("is not in the stroke store"));
    }
}
