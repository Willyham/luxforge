//! The panorama provider's typed refusals. Each names the frames it refuses by their position in
//! the request (shown from 1). Registration refuses with the first four; stitching adds its own.

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
