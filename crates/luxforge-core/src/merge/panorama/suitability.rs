//! Whether frames can be a panorama from what they say of themselves (`docs/design/panorama-merge.md`,
//! "Validating inputs"): what `merge.plan` judges from headers alone and the job judges again from
//! the decoded frames, before any pixel is registered. Registration adds the refusals that need
//! pixels ([`super::Refusal`]); [`named`] words both by the frames' file names.
//!
//! - **Count and duplicates.** 2–32 frames; no file twice, and no RAW and JPEG of one exposure.
//! - **Recording.** Every frame RAW or every frame JPEG: a mixture is refused naming the first
//!   frame of the other kind than the first frame's.
//! - **Camera and lens.** Make and model, recording mode, monochrome or colour, upright dimensions
//!   and focal length (within 1 mm or 2% of the first frame's) match the first frame's. A value
//!   recorded on one frame and not another is a mixture; values absent on every frame agree.
//! - **Exposure.** Shutter, aperture and ISO may differ, but frames whose settings put them more
//!   than [`MAX_SPREAD_EV`] apart are a bracket, not a panorama.
use super::registration::{MAX_FRAMES, MIN_FRAMES};
use crate::{
    Error,
    merge::hdr::{
        FrameMetadata, Recording,
        frame::{same_exposure, same_focal, same_option},
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Write as _;

/// The widest spread of the frames' exposures from their settings, in stops, a panorama accepts.
pub const MAX_SPREAD_EV: f64 = 3.0;

/// Why frames cannot be a panorama from their headers or decoded interpretation. Positions are in
/// the stack's order; `first` is the earlier.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Unsuitable {
    InputCount {
        count: usize,
    },
    RepeatedInput {
        first: usize,
        second: usize,
    },
    SameExposure {
        first: usize,
        second: usize,
    },
    /// The first frame of the other recording than the first frame's.
    MixedRecording {
        input: usize,
    },
    MixedCameras {
        input: usize,
    },
    MixedRecordingModes {
        input: usize,
    },
    MixedColour {
        input: usize,
    },
    MixedDimensions {
        input: usize,
    },
    MixedFocalLength {
        input: usize,
    },
    /// The frames' settings put `first` and `second` `spread_ev` stops apart.
    Bracket {
        first: usize,
        second: usize,
        spread_ev: f64,
    },
}

impl Unsuitable {
    /// The refusal as an error naming the frames by `names` (their file names in the stack's
    /// order): `validation`, its data the typed reason, the positions, the names and `kind`.
    pub fn error(&self, names: &[String]) -> Error {
        let name = |at: &usize| {
            names
                .get(*at)
                .cloned()
                .unwrap_or_else(|| format!("frame {}", at + 1))
        };
        let message = match self {
            Self::InputCount { count } => {
                format!("a panorama merge takes {MIN_FRAMES} to {MAX_FRAMES} photos, not {count}")
            }
            Self::RepeatedInput { first, second } => {
                format!("{} and {} are the same file", name(first), name(second))
            }
            Self::SameExposure { first, second } => format!(
                "{} and {} are a RAW and a JPEG of one exposure",
                name(first),
                name(second)
            ),
            Self::MixedRecording { input } => format!(
                "{} is not the same kind of file as the first frame; a panorama's frames are all \
                 RAW or all JPEG",
                name(input)
            ),
            Self::MixedCameras { input } => format!("{} is from a different camera", name(input)),
            Self::MixedRecordingModes { input } => {
                format!("{} uses a different RAW recording mode", name(input))
            }
            Self::MixedColour { input } => {
                format!("{} mixes monochrome and colour", name(input))
            }
            Self::MixedDimensions { input } => {
                format!("{} has different dimensions", name(input))
            }
            Self::MixedFocalLength { input } => {
                format!("{} uses a different focal length", name(input))
            }
            Self::Bracket {
                first,
                second,
                spread_ev,
            } => format!(
                "{} and {} are {spread_ev:.1} stops apart: an exposure bracket, not a panorama; \
                 merge HDR instead",
                name(first),
                name(second)
            ),
        };
        let mut data = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(object) = data.as_object_mut() {
            object.insert("kind".into(), "panorama".into());
            object.insert("names".into(), json!(names));
        }
        Error::validation(message).with_data(data)
    }
}

/// Each frame's exposure from its settings, in stops (`log2(t · ISO / N²)`), with each term used
/// only when every frame records it; none when no frame records any.
pub fn settings_ev(frames: &[FrameMetadata]) -> Option<Vec<f64>> {
    let all = |term: &dyn Fn(&FrameMetadata) -> Option<f64>| {
        frames
            .iter()
            .map(|frame| term(frame).filter(|value| value.is_finite() && *value > 0.0))
            .collect::<Option<Vec<f64>>>()
    };
    let time = all(&|frame| frame.exposure.shutter_s);
    let iso = all(&|frame| frame.exposure.iso.map(f64::from));
    let aperture = all(&|frame| frame.exposure.f_number);
    if time.is_none() && iso.is_none() && aperture.is_none() {
        return None;
    }
    Some(
        (0..frames.len())
            .map(|at| {
                time.as_ref().map_or(0.0, |time| time[at].log2())
                    + iso.as_ref().map_or(0.0, |iso| iso[at].log2())
                    - aperture
                        .as_ref()
                        .map_or(0.0, |aperture| 2.0 * aperture[at].log2())
            })
            .collect(),
    )
}

/// Each frame's scale to `reference`'s exposure from its settings (`2^(ev_ref − ev)`), for RAW
/// frames; 1 for every frame when the settings say nothing or the frames are JPEGs, whose
/// in-camera rendering is not linear in exposure.
pub fn exposure_scales(frames: &[FrameMetadata], reference: usize) -> Vec<f64> {
    let raw = frames.iter().all(FrameMetadata::is_raw);
    match settings_ev(frames).filter(|_| raw) {
        Some(ev) => {
            let at = ev.get(reference).copied().unwrap_or(0.0);
            ev.iter().map(|ev| (at - ev).exp2()).collect()
        }
        None => vec![1.0; frames.len()],
    }
}

/// Whether `frames`, in the stack's order, can be a panorama: see the module documentation. Frames
/// whose sizes are unknown (0) are not compared by size.
pub fn check(frames: &[FrameMetadata]) -> Result<(), Unsuitable> {
    if !(MIN_FRAMES..=MAX_FRAMES).contains(&frames.len()) {
        return Err(Unsuitable::InputCount {
            count: frames.len(),
        });
    }
    for (first, a) in frames.iter().enumerate() {
        for (second, b) in frames.iter().enumerate().skip(first + 1) {
            if a.identity == b.identity {
                return Err(Unsuitable::RepeatedInput { first, second });
            }
            if a.is_raw() != b.is_raw() && same_exposure(a, b) {
                return Err(Unsuitable::SameExposure { first, second });
            }
        }
    }
    let first = &frames[0];
    if let Some(input) = frames
        .iter()
        .position(|frame| frame.is_raw() != first.is_raw())
    {
        return Err(Unsuitable::MixedRecording { input });
    }
    for (input, frame) in frames.iter().enumerate().skip(1) {
        if frame.make != first.make || frame.model != first.model {
            return Err(Unsuitable::MixedCameras { input });
        }
        if let (
            Recording::Raw { mode, layout },
            Recording::Raw {
                mode: first_mode,
                layout: first_layout,
            },
        ) = (&frame.recording, &first.recording)
        {
            if mode != first_mode {
                return Err(Unsuitable::MixedRecordingModes { input });
            }
            if layout.is_monochrome() != first_layout.is_monochrome() {
                return Err(Unsuitable::MixedColour { input });
            }
        }
        if frame.width > 0
            && first.width > 0
            && (frame.width, frame.height) != (first.width, first.height)
        {
            return Err(Unsuitable::MixedDimensions { input });
        }
        if !same_option(frame.exposure.focal_mm, first.exposure.focal_mm, same_focal) {
            return Err(Unsuitable::MixedFocalLength { input });
        }
    }
    if let Some(ev) = settings_ev(frames) {
        let (low, high) = ev
            .iter()
            .enumerate()
            .fold((0, 0), |(low, high), (at, value)| {
                (
                    if *value < ev[low] { at } else { low },
                    if *value > ev[high] { at } else { high },
                )
            });
        let spread_ev = ev[high] - ev[low];
        if spread_ev > MAX_SPREAD_EV + 1e-9 {
            return Err(Unsuitable::Bracket {
                first: low.min(high),
                second: low.max(high),
                spread_ev,
            });
        }
    }
    Ok(())
}

/// A registration or stitch refusal ([`super::Refusal`], or any error whose data names frames by
/// request position) worded by the frames' file names in request order, with the names in its
/// data: "DSC_0004.NEF: shares no detail with the other frames". Any other error is unchanged.
pub fn named(error: Error, names: &[String]) -> Error {
    let Some(data) = error.data.as_deref().cloned() else {
        return error;
    };
    if data.get("kind").and_then(Value::as_str) != Some("panorama") || data.get("names").is_some() {
        return error;
    }
    let name = |at: u64| {
        names
            .get(at as usize)
            .cloned()
            .unwrap_or_else(|| format!("frame {}", at + 1))
    };
    let frames: Vec<String> = data
        .get("frames")
        .and_then(Value::as_array)
        .map(|frames| frames.iter().filter_map(Value::as_u64).map(name).collect())
        .unwrap_or_default();
    let pair = data
        .get("first")
        .and_then(Value::as_u64)
        .zip(data.get("second").and_then(Value::as_u64))
        .map(|(first, second)| (name(first), name(second)));
    let list = |frames: &[String]| match frames {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    };
    let detail = match (data.get("reason").and_then(Value::as_str), pair) {
        (Some("same-view"), Some((first, second))) => {
            format!("{first} and {second} are the same view; merge HDR instead")
        }
        (Some("low-texture"), _) if !frames.is_empty() => format!(
            "{} {} too little detail to register",
            list(&frames),
            if frames.len() == 1 { "has" } else { "have" }
        ),
        (Some("disconnected"), _) if !frames.is_empty() => format!(
            "{}: {} no detail with the other frames",
            list(&frames),
            if frames.len() == 1 { "shares" } else { "share" }
        ),
        (Some("ambiguous-registration"), _) if !frames.is_empty() => format!(
            "{}: {} detail with the other frames but {} not register as one turning camera",
            list(&frames),
            if frames.len() == 1 { "shares" } else { "share" },
            if frames.len() == 1 { "does" } else { "do" }
        ),
        _ => {
            // A refusal this module does not word: its own message, each "frame N" its name.
            let mut detail = String::new();
            let mut rest = error.detail.as_str();
            while let Some(at) = rest.find("frame ") {
                let (head, tail) = rest.split_at(at);
                detail.push_str(head);
                let digits: String = tail[6..].chars().take_while(char::is_ascii_digit).collect();
                match digits.parse::<u64>() {
                    Ok(number) if number >= 1 => {
                        let _ = write!(detail, "{}", name(number - 1));
                        rest = &tail[6 + digits.len()..];
                    }
                    _ => {
                        detail.push_str("frame ");
                        rest = &tail[6..];
                    }
                }
            }
            detail.push_str(rest);
            detail
        }
    };
    let mut data = data;
    if let Some(object) = data.as_object_mut() {
        object.insert("names".into(), json!(names));
    }
    Error::new(error.kind, detail).with_data(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::hdr::{ExposureSettings, SensorLayout};

    fn frame(identity: &str, raw: bool, shutter: f64) -> FrameMetadata {
        FrameMetadata {
            identity: identity.into(),
            make: Some("NIKON CORPORATION".into()),
            model: Some("NIKON Z 6".into()),
            recording: if raw {
                Recording::Raw {
                    mode: "file:nef".into(),
                    layout: SensorLayout::Bayer,
                }
            } else {
                Recording::Jpeg
            },
            width: 6048,
            height: 4024,
            orientation: 1,
            exposure: ExposureSettings {
                shutter_s: Some(shutter),
                f_number: Some(8.0),
                iso: Some(100),
                bias_ev: None,
                focal_mm: Some(24.0),
            },
            captured_at_ms: Some(identity.len() as i64),
        }
    }

    fn names(count: usize) -> Vec<String> {
        (1..=count).map(|at| format!("DSC_{at:04}.NEF")).collect()
    }

    #[test]
    fn panorama_merge_suitability_names_each_refusal_by_file() {
        let row: Vec<_> = (0..4)
            .map(|at| frame(&"f".repeat(at + 1), true, 1.0 / 250.0))
            .collect();
        check(&row).unwrap();
        // Automatic exposure within three stops is a panorama.
        let mut auto = row.clone();
        auto[3].exposure.shutter_s = Some(8.0 / 250.0);
        check(&auto).unwrap();
        assert_eq!(exposure_scales(&auto, 0)[3], 0.125);

        let mut bracket = row.clone();
        bracket[2].exposure.shutter_s = Some(16.0 / 250.0);
        let refusal = check(&bracket).unwrap_err();
        assert!(matches!(
            refusal,
            Unsuitable::Bracket {
                first: 0,
                second: 2,
                ..
            }
        ));
        let error = refusal.error(&names(4));
        assert_eq!(
            error.detail,
            "DSC_0001.NEF and DSC_0003.NEF are 4.0 stops apart: an exposure bracket, not a \
             panorama; merge HDR instead"
        );
        assert_eq!(error.data.as_ref().unwrap()["kind"], "panorama");
        assert_eq!(error.data.as_ref().unwrap()["reason"], "bracket");

        let mut mixed = row.clone();
        // Another exposure recorded as JPEG: not a RAW+JPEG pair, but a mixed set.
        mixed[2] = frame("jpeg", false, 1.0 / 125.0);
        assert_eq!(check(&mixed), Err(Unsuitable::MixedRecording { input: 2 }));
        let mut repeated = row.clone();
        repeated[3].identity = row[1].identity.clone();
        assert_eq!(
            check(&repeated),
            Err(Unsuitable::RepeatedInput {
                first: 1,
                second: 3
            })
        );
        let mut pair = row.clone();
        pair[1] = FrameMetadata {
            recording: Recording::Jpeg,
            identity: "pair".into(),
            ..row[0].clone()
        };
        assert_eq!(
            check(&pair),
            Err(Unsuitable::SameExposure {
                first: 0,
                second: 1
            })
        );
        let mut zoomed = row.clone();
        zoomed[1].exposure.focal_mm = Some(24.4);
        check(&zoomed).unwrap();
        zoomed[1].exposure.focal_mm = Some(26.0);
        assert_eq!(
            check(&zoomed),
            Err(Unsuitable::MixedFocalLength { input: 1 })
        );
        let mut unrecorded = row.clone();
        unrecorded[2].exposure.focal_mm = None;
        assert_eq!(
            check(&unrecorded),
            Err(Unsuitable::MixedFocalLength { input: 2 })
        );
        let mut other = row.clone();
        other[1].model = Some("NIKON Z 7".into());
        assert_eq!(check(&other), Err(Unsuitable::MixedCameras { input: 1 }));
        let mut sized = row.clone();
        sized[3].width = 4000;
        assert_eq!(check(&sized), Err(Unsuitable::MixedDimensions { input: 3 }));
        assert_eq!(check(&row[..1]), Err(Unsuitable::InputCount { count: 1 }));
    }

    #[test]
    fn panorama_merge_registration_refusals_are_worded_by_file_name() {
        let names = names(5);
        let error = named(
            super::super::Refusal::Disconnected { frames: vec![3] }.into(),
            &names,
        );
        assert_eq!(
            error.detail,
            "DSC_0004.NEF: shares no detail with the other frames"
        );
        assert_eq!(error.data.as_ref().unwrap()["names"][3], "DSC_0004.NEF");
        let error = named(
            super::super::Refusal::SameView {
                first: 0,
                second: 4,
            }
            .into(),
            &names,
        );
        assert_eq!(
            error.detail,
            "DSC_0001.NEF and DSC_0005.NEF are the same view; merge HDR instead"
        );
        let unknown = Error::validation("frame 2 is outside the projection's field")
            .with_data(json!({"kind": "panorama", "reason": "excessive-field-of-view"}));
        assert_eq!(
            named(unknown, &names).detail,
            "DSC_0002.NEF is outside the projection's field"
        );
        let other = Error::validation("unrelated");
        assert_eq!(named(other, &names).detail, "unrelated");
    }
}
