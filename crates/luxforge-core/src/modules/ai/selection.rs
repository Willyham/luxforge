//! A fill's region from its request: the picked objects, the selection, a Corrections brush
//! stroke, and the optional effect region, a second stroke for what the object casts (a shadow, a
//! reflection). Each stroke is captured exactly as a correction's stroke is
//! ([`CorrectionStroke::capture`]: the same posted positions, decimation, grid and radius range)
//! and becomes coverage as a correction's footprint does, with the mask brush's profile at full
//! opacity: the design's Nothing case, the stroke's footprint as painted. Each picked object is a
//! `mask-alpha` artifact `task.select-object` published, whose removal region is
//! [`super::object_region`]'s. The region is the largest of the coverages at every pixel.
//!
//! **The feather lies outside what was painted.** Coverage is full over the footprint as painted
//! and falls to zero over 15% of the radius past it, as the prototype's footprint region and the
//! object region's soft outer edge do. A correction's brush feathers inward, which a removal must
//! not: what a person paints over is the object to take away, and an object edge under a partial
//! coverage `c` shows through the composite `in + c · (patch − in)` at `1 − c`, a faint trace of
//! the object inside the fill wherever its edge lies near the brush's.
use super::{
    fill::Coverage,
    object_region::{self, ObjectRaster},
};
use crate::{
    ArtifactId, Error,
    modules::{Region, Stage, corrections::CorrectionStroke},
    repair::footprint::{self, Footprint},
};
use serde_json::{Map, Value};

/// The feather of a brush selection's footprint past its painted edge, as a share of its radius in
/// percent.
pub(crate) const SELECTION_FEATHER: f64 = 15.0;

/// The radius a stroke's coverage reaches, in content heights: the painted radius and the feather
/// past it.
fn reach(stroke: &CorrectionStroke) -> f64 {
    stroke.radius() * (1.0 + SELECTION_FEATHER / 100.0)
}

/// The feather as a share of [`reach`] in percent, so its band is [`SELECTION_FEATHER`] percent
/// of the painted radius and coverage is full out to the painted edge.
const REACH_FEATHER: f64 = 100.0 * SELECTION_FEATHER / (100.0 + SELECTION_FEATHER);

/// The selection's path and radius.
pub(crate) const SELECTION_POINTS: &str = "selection-points";
pub(crate) const SELECTION_RADIUS: &str = "selection-radius";
/// The effect region's path and radius; its radius defaults to the selection's.
pub(crate) const EFFECT_POINTS: &str = "effect-points";
pub(crate) const EFFECT_RADIUS: &str = "effect-radius";
/// The picked objects: `mask-alpha` artifacts `task.select-object` published.
pub(crate) const OBJECT_ARTIFACTS: &str = "object-artifacts";
/// The most objects one fill removes.
pub(crate) const MAX_OBJECTS: usize = 8;

/// The strokes of one request, captured, and the objects it names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Selection {
    strokes: Vec<CorrectionStroke>,
    objects: Vec<ArtifactId>,
}

fn points(parameters: &Map<String, Value>, name: &str) -> Result<Option<Vec<[f64; 2]>>, Error> {
    parameters
        .get(name)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| Error::validation(format!("invalid {name}: {error}")))
        })
        .transpose()
}

impl Selection {
    /// The region a request names: the picked objects, the selection stroke with its radius, or
    /// both, and the effect region when it posts a path, its radius defaulting to the selection's.
    /// A radius without its path is refused, and so is a request that names neither objects nor a
    /// selection.
    pub(crate) fn parse(parameters: &Map<String, Value>) -> Result<Self, Error> {
        let radius = |name: &str| parameters.get(name).and_then(Value::as_f64);
        let objects = match parameters.get(OBJECT_ARTIFACTS) {
            None => Vec::new(),
            Some(value) => {
                serde_json::from_value::<Vec<ArtifactId>>(value.clone()).map_err(|error| {
                    Error::validation(format!("invalid {OBJECT_ARTIFACTS}: {error}"))
                })?
            }
        };
        if objects.len() > MAX_OBJECTS {
            return Err(Error::validation(format!(
                "{OBJECT_ARTIFACTS} names at most {MAX_OBJECTS} objects, not {}",
                objects.len()
            )));
        }
        let mut strokes = Vec::new();
        let selection_radius = radius(SELECTION_RADIUS);
        match (points(parameters, SELECTION_POINTS)?, selection_radius) {
            (Some(selection), Some(selection_radius)) => {
                strokes.push(CorrectionStroke::capture(&selection, selection_radius)?);
            }
            (Some(_), None) => {
                return Err(Error::validation(format!(
                    "{SELECTION_POINTS} needs {SELECTION_RADIUS}"
                )));
            }
            (None, Some(_)) => {
                return Err(Error::validation(format!(
                    "{SELECTION_RADIUS} needs {SELECTION_POINTS}"
                )));
            }
            (None, None) if objects.is_empty() => {
                return Err(Error::validation(format!(
                    "name {SELECTION_POINTS} with {SELECTION_RADIUS}, {OBJECT_ARTIFACTS}, or both"
                )));
            }
            (None, None) => {}
        }
        match points(parameters, EFFECT_POINTS)? {
            Some(effect) => {
                let effect_radius =
                    radius(EFFECT_RADIUS).or(selection_radius).ok_or_else(|| {
                        Error::validation(format!(
                            "{EFFECT_RADIUS} is required without {SELECTION_RADIUS}"
                        ))
                    })?;
                strokes.push(CorrectionStroke::capture(&effect, effect_radius)?);
            }
            None if parameters.contains_key(EFFECT_RADIUS) => {
                return Err(Error::validation(format!(
                    "{EFFECT_RADIUS} needs {EFFECT_POINTS}"
                )));
            }
            None => {}
        }
        Ok(Self { strokes, objects })
    }

    /// The picked objects the request names, in its order.
    pub(crate) fn objects(&self) -> &[ArtifactId] {
        &self.objects
    }

    fn positions(stroke: &CorrectionStroke) -> Vec<[f64; 2]> {
        stroke.points().collect()
    }

    /// Every pixel of `stage` whose coverage may be above zero, `O(segments + objects)`: what the
    /// owner admits a request by before anything is read. `objects` are the rasters of
    /// [`Self::objects`], as `task.select-object` recorded them.
    pub(crate) fn support(&self, stage: Stage, objects: &[ObjectRaster]) -> Region {
        self.stroke_support(stage)
            .into_iter()
            .chain(
                objects
                    .iter()
                    .map(|raster| object_region::support(*raster, stage)),
            )
            .filter(|support| !support.is_empty())
            .reduce(super::fill::crop::union)
            .unwrap_or(Region::EMPTY)
    }

    /// The strokes' support alone.
    fn stroke_support(&self, stage: Stage) -> Option<Region> {
        self.strokes
            .iter()
            .map(|stroke| {
                footprint::support(
                    &footprint::segments(&Self::positions(stroke), stage),
                    reach(stroke),
                    stage,
                )
            })
            .filter(|support| !support.is_empty())
            .reduce(super::fill::crop::union)
    }

    /// The request's region over `stage`: the strokes' coverage and the removal region of each of
    /// `objects`, the rasters of [`Self::objects`] as `(codes, width, height)`, joined by their
    /// maximum at every pixel over the tight bounds of the pixels above zero; `None` when nothing
    /// is covered. `checkpoint` is called between bands of rows.
    pub(crate) fn region(
        &self,
        stage: Stage,
        objects: &[(&[u8], u32, u32)],
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Option<Coverage>, Error> {
        let mut parts: Vec<Coverage> = self.coverage(stage)?.into_iter().collect();
        for &(codes, width, height) in objects {
            parts.extend(object_region::region(
                codes, width, height, stage, checkpoint,
            )?);
        }
        Ok(Coverage::join(parts))
    }

    /// The strokes' coverage over `stage`, over the tight bounds of its pixels above zero; `None`
    /// when they cover no pixel or the request names none.
    pub(crate) fn coverage(&self, stage: Stage) -> Result<Option<Coverage>, Error> {
        let Some(support) = self.stroke_support(stage) else {
            return Ok(None);
        };
        let footprints: Vec<Footprint> = self
            .strokes
            .iter()
            .map(|stroke| {
                Footprint::new(
                    &Self::positions(stroke),
                    reach(stroke),
                    REACH_FEATHER,
                    100.0,
                    stage,
                )
            })
            .collect();
        let mut values = Vec::with_capacity(support.pixels() as usize);
        for y in support.y0..support.y1() {
            for x in support.x0..support.x1() {
                let c = footprints
                    .iter()
                    .map(|footprint| footprint.coverage(x, y))
                    .fold(0.0_f64, f64::max);
                values.push(c as f32);
            }
        }
        Ok(Coverage::new(support, values)?.tightened())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(parameters: Value) -> Result<Selection, Error> {
        Selection::parse(parameters.as_object().unwrap())
    }

    fn refused(parameters: Value, words: &str) {
        let error = parse(parameters).unwrap_err();
        assert_eq!(error.kind.code(), "validation", "{error:?}");
        assert!(error.detail.contains(words), "{error:?}");
    }

    fn artifact(n: u32) -> String {
        format!("artifact-{n:064x}")
    }

    #[test]
    fn a_request_names_objects_a_selection_or_both_and_each_radius_with_its_path() {
        let objects = parse(json!({OBJECT_ARTIFACTS: [artifact(1), artifact(2)]})).unwrap();
        assert_eq!(objects.objects().len(), 2);
        let s = Stage {
            width: 64,
            height: 64,
        };
        assert!(objects.coverage(s).unwrap().is_none());
        let both = parse(json!({
            OBJECT_ARTIFACTS: [artifact(1)],
            SELECTION_POINTS: [[0.5, 0.5]],
            SELECTION_RADIUS: 0.05,
        }))
        .unwrap();
        assert_eq!((both.objects().len(), both.strokes.len()), (1, 1));
        // An effect stroke beside objects alone takes its own radius.
        let effect = parse(json!({
            OBJECT_ARTIFACTS: [artifact(1)],
            EFFECT_POINTS: [[0.5, 0.6]],
            EFFECT_RADIUS: 0.05,
        }))
        .unwrap();
        assert_eq!(effect.strokes.len(), 1);
        refused(
            json!({OBJECT_ARTIFACTS: [artifact(1)], EFFECT_POINTS: [[0.5, 0.6]]}),
            "effect-radius is required",
        );
        refused(json!({}), "name selection-points");
        refused(
            json!({SELECTION_POINTS: [[0.5, 0.5]]}),
            "needs selection-radius",
        );
        refused(
            json!({OBJECT_ARTIFACTS: [artifact(1)], SELECTION_RADIUS: 0.05}),
            "needs selection-points",
        );
        let nine: Vec<String> = (0..9).map(artifact).collect();
        refused(json!({OBJECT_ARTIFACTS: nine}), "at most 8 objects");
        refused(
            json!({OBJECT_ARTIFACTS: ["not-an-artifact"]}),
            "invalid object-artifacts",
        );
    }

    #[test]
    fn the_support_holds_every_object_beside_the_strokes() {
        let s = Stage {
            width: 400,
            height: 400,
        };
        let selection = parse(json!({
            OBJECT_ARTIFACTS: [artifact(1)],
            SELECTION_POINTS: [[0.2, 0.2]],
            SELECTION_RADIUS: 0.02,
        }))
        .unwrap();
        let raster = ObjectRaster {
            width: 100,
            height: 100,
            nonzero: Some([80, 80, 90, 90]),
        };
        let strokes = selection.support(s, &[]);
        let joined = selection.support(s, &[raster]);
        let object = object_region::support(raster, s);
        for part in [strokes, object] {
            assert!(
                joined.x0 <= part.x0
                    && joined.y0 <= part.y0
                    && joined.x1() >= part.x1()
                    && joined.y1() >= part.y1(),
                "{joined:?} does not hold {part:?}"
            );
        }
        assert!(joined.x1() > strokes.x1() && joined.y1() > strokes.y1());
    }
}
