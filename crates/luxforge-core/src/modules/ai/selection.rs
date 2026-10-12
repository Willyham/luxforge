//! A fill's region from its request: the selection, a Corrections brush stroke, and the optional
//! effect region, a second stroke for what the object casts (a shadow, a reflection). Each is
//! captured exactly as a correction's stroke is ([`CorrectionStroke::capture`]: the same posted
//! positions, decimation, grid and radius range) and becomes coverage as a correction's footprint
//! does, with the mask brush's profile at full opacity: the design's Nothing case, the stroke's
//! footprint as painted. The region is the larger of the two coverages at every pixel.
//!
//! **The feather lies outside what was painted.** Coverage is full over the footprint as painted
//! and falls to zero over 15% of the radius past it, as the prototype's footprint region and the
//! object region's soft outer edge do. A correction's brush feathers inward, which a removal must
//! not: what a person paints over is the object to take away, and an object edge under a partial
//! coverage `c` shows through the composite `in + c · (patch − in)` at `1 − c`, a faint trace of
//! the object inside the fill wherever its edge lies near the brush's.
use super::fill::Coverage;
use crate::{
    Error,
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

/// The strokes of one request, captured.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Selection {
    strokes: Vec<CorrectionStroke>,
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
    /// The strokes a request names: the selection, required, and the effect region when it posts
    /// a path. An effect radius without an effect path is refused.
    pub(crate) fn parse(parameters: &Map<String, Value>) -> Result<Self, Error> {
        let radius = |name: &str| parameters.get(name).and_then(Value::as_f64);
        let selection = points(parameters, SELECTION_POINTS)?
            .ok_or_else(|| Error::validation(format!("{SELECTION_POINTS} is required")))?;
        let selection_radius = radius(SELECTION_RADIUS)
            .ok_or_else(|| Error::validation(format!("{SELECTION_RADIUS} is required")))?;
        let mut strokes = vec![CorrectionStroke::capture(&selection, selection_radius)?];
        match points(parameters, EFFECT_POINTS)? {
            Some(effect) => strokes.push(CorrectionStroke::capture(
                &effect,
                radius(EFFECT_RADIUS).unwrap_or(selection_radius),
            )?),
            None if parameters.contains_key(EFFECT_RADIUS) => {
                return Err(Error::validation(format!(
                    "{EFFECT_RADIUS} needs {EFFECT_POINTS}"
                )));
            }
            None => {}
        }
        Ok(Self { strokes })
    }

    fn positions(stroke: &CorrectionStroke) -> Vec<[f64; 2]> {
        stroke.points().collect()
    }

    /// Every pixel of `stage` whose coverage may be above zero, `O(segments)`: what the owner
    /// admits a request by before anything is read.
    pub(crate) fn support(&self, stage: Stage) -> Region {
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
            .unwrap_or(Region::EMPTY)
    }

    /// The region's coverage over `stage`, over the tight bounds of its pixels above zero;
    /// `None` when it covers no pixel.
    pub(crate) fn coverage(&self, stage: Stage) -> Result<Option<Coverage>, Error> {
        let support = self.support(stage);
        if support.is_empty() {
            return Ok(None);
        }
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
