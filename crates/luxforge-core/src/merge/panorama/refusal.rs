//! The panorama provider's typed refusals. Registration's name the frames they refuse by their
//! position in the request (shown from 1); stitching's name the projection or sizes at fault.

use super::projection::Projection;
use crate::Error;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Why frames cannot be merged into a panorama.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Refusal {
    /// These frames have too few usable features to register: blank, flat, blurred or clipped.
    LowTexture { frames: Vec<usize> },
    /// These frames share no verified detail with the rest: nothing connects them to the largest
    /// group of frames that do.
    Disconnected { frames: Vec<usize> },
    /// These frames share verified detail with the rest, but no turn of one camera about its
    /// centre, with the shared focal length and radial term, registers them within the inlier
    /// threshold: a zoom, parallax or a moving scene.
    AmbiguousRegistration { frames: Vec<usize> },
    /// Two frames register within 2% of the frame's diagonal of each other and overlap by at least
    /// 90%: a bracket, not a panorama.
    SameView { first: usize, second: usize },
    /// The registered field is wider or taller than the projection holds: Perspective past 60°
    /// from its centre, Cylindrical past 70° from the horizon. Fields in whole degrees.
    ExcessiveFieldOfView {
        projection: Projection,
        horizontal_deg: u32,
        vertical_deg: u32,
    },
    /// The stitch fits the output limits only below the smallest scale a merge accepts. Its
    /// bounding rectangle at full scale.
    OutputTooLarge { width: u64, height: u64 },
    /// The covered rectangle is under a quarter of the stitch's bounding rectangle on a side.
    InsufficientCoverage {
        width: u32,
        height: u32,
        bounding_width: u32,
        bounding_height: u32,
    },
}

fn projection_name(projection: Projection) -> &'static str {
    match projection {
        Projection::Perspective => "Perspective",
        Projection::Cylindrical => "Cylindrical",
        Projection::Spherical => "Spherical",
    }
}

fn list(frames: &[usize]) -> String {
    let names: Vec<String> = frames.iter().map(|f| (f + 1).to_string()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => format!("frame {one}"),
        [init @ .., last] => format!("frames {} and {last}", init.join(", ")),
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LowTexture { frames } => {
                write!(f, "{} has too little detail to register", list(frames))
            }
            Self::Disconnected { frames } => {
                write!(
                    f,
                    "{}: shares no detail with the other frames",
                    list(frames)
                )
            }
            Self::AmbiguousRegistration { frames } => write!(
                f,
                "{}: shares detail with the other frames but does not register as one turning \
                 camera",
                list(frames)
            ),
            Self::SameView { first, second } => write!(
                f,
                "frames {} and {} are the same view; merge HDR instead",
                first + 1,
                second + 1
            ),
            Self::ExcessiveFieldOfView {
                projection,
                horizontal_deg,
                vertical_deg,
            } => {
                let suggestion = match projection {
                    Projection::Perspective => "Cylindrical",
                    _ => "Spherical",
                };
                write!(
                    f,
                    "the frames span {horizontal_deg}° × {vertical_deg}°, too much for {}; \
                     choose {suggestion}",
                    projection_name(*projection)
                )
            }
            Self::OutputTooLarge { width, height } => write!(
                f,
                "the panorama would be {width} × {height} pixels, too large to save even scaled \
                 down"
            ),
            Self::InsufficientCoverage {
                width,
                height,
                bounding_width,
                bounding_height,
            } => write!(
                f,
                "the frames cover only a {width} × {height} rectangle of the {bounding_width} × \
                 {bounding_height} panorama; try Boundary Warp"
            ),
        }
    }
}

impl Refusal {
    /// The typed refusal as a refusal's data carries it: its `reason` and the frames it names,
    /// with the merge kind it refuses, `panorama`.
    pub fn data(&self) -> serde_json::Value {
        let mut data = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(object) = data.as_object_mut() {
            object.insert("kind".into(), "panorama".into());
        }
        data
    }

    /// The typed refusal an error carries, if it carries one.
    pub fn of(error: &Error) -> Option<Self> {
        let data = error.data.as_ref()?;
        (data.get("kind")?.as_str()? == "panorama")
            .then(|| serde_json::from_value((**data).clone()).ok())
            .flatten()
    }
}

impl From<Refusal> for Error {
    /// A refusal is the request's fault: a validation failure whose `data` carries the typed
    /// refusal and its kind ([`Refusal::data`]).
    fn from(refusal: Refusal) -> Self {
        Error::validation(refusal.to_string()).with_data(refusal.data())
    }
}
