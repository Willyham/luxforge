//! The `lens-profiles` answer. What the photo's identity detects, what the layer applies and a
//! search, with the query-choice vocabulary's cards, notice and report filled in here, so every
//! client shows the same state and the desktop decides nothing about lenses.
use super::{
    index::LensIndex,
    payload::{self, FocalSource},
    products, report,
    resolve::{self, Candidate, Reason, ResolveInput},
};
use crate::SourceOptics;
use luxforge_raw::OpticalStatus;
use serde::Serialize;
use serde_json::{Map, Value, json};

/// The section's one state, in precedence order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum State {
    /// The layer holds a profile.
    Applied,
    /// The source already corrects distortion, so no profile may be added.
    AlreadyCorrected,
    /// The database holds no record of this camera, so no profile can fit it.
    CameraNotFound,
    /// A profile matches the photo's identity; [`Candidate::eligible`] says whether it applies
    /// as it stands.
    Detected,
    /// The camera is known but no profile matches its lens.
    LensNotFound,
}

pub(crate) struct Request<'a> {
    pub text: &'a str,
    pub page: usize,
    pub assume: bool,
}

fn status_word(status: OpticalStatus) -> &'static str {
    match status {
        OpticalStatus::Applied => "corrected",
        OpticalStatus::KnownUnapplied => "not corrected",
        OpticalStatus::Unknown => "unknown",
    }
}

fn millimetres(focal: f64) -> String {
    format!("{focal} mm")
}

/// A card's title and subtitle: a fixed lens is named by its camera, an interchangeable lens by
/// itself with its camera beneath.
fn card_text(fixed: bool, camera: &str, lens: &str, focal: Option<f64>) -> (String, String) {
    let (title, mut subtitle) = if fixed {
        (format!("{camera} · built-in lens"), lens.to_owned())
    } else {
        (lens.to_owned(), camera.to_owned())
    };
    if let Some(focal) = focal {
        subtitle.push_str(&format!(" · {}", millimetres(focal)));
    }
    (title, subtitle)
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn notice(level: &str, text: impl Into<String>) -> Value {
    json!({"level": level, "text": text.into()})
}

pub(crate) fn answer(
    index: &LensIndex,
    optics: &SourceOptics,
    input: &ResolveInput,
    applied: Option<&payload::Profile>,
    request: &Request<'_>,
) -> Value {
    let detection = resolve::detect(index, input);
    let identity = &optics.identity;
    let ledger = &optics.ledger;
    let camera_name = products::camera_name(
        identity.make.as_deref(),
        identity.model.as_deref(),
        detection.camera,
    );
    let distortion = ledger.distortion.status;
    let focal = input.focal_override.or(input.focal_mm);
    let mut reasons = detection.reasons.clone();
    match distortion {
        OpticalStatus::Applied => reasons.push(Reason::EmbeddedDistortionApplied),
        OpticalStatus::Unknown if !request.assume => {
            reasons.push(Reason::AssumeUncorrectedRequired);
        }
        _ => {}
    }
    reasons.sort();
    reasons.dedup();
    let state = if applied.is_some() {
        State::Applied
    } else if distortion == OpticalStatus::Applied {
        State::AlreadyCorrected
    } else if detection.camera.is_none() {
        State::CameraNotFound
    } else if detection.detected.is_some() {
        State::Detected
    } else {
        State::LensNotFound
    };
    // A condition of the photo no person can change: no profile could ever be selected.
    let unselectable = matches!(state, State::AlreadyCorrected | State::CameraNotFound)
        || detection
            .photo
            .iter()
            .any(|r| matches!(r, Reason::CropModeMismatch | Reason::UnsupportedStage));
    let search = !unselectable;
    // A selection on a photo whose in-camera correction is unknown carries the acknowledgement,
    // which the card's note and the notice state; the action still refuses a request without it.
    let parameters = if distortion == OpticalStatus::Unknown {
        Map::from_iter([("assume-uncorrected".to_owned(), Value::Bool(true))])
    } else {
        Map::new()
    };
    let unknown_note = if ledger.interpretation == "jpeg" {
        "The camera may already have corrected distortion in this JPEG; applying assumes it did not."
    } else {
        "Whether the source already corrects distortion is unknown; applying assumes it does not."
    };

    let mut page = resolve::search(index, input, request.text, request.page);
    for row in &mut page.rows {
        if distortion == OpticalStatus::Applied {
            row.eligible = false;
            row.reasons.push(Reason::EmbeddedDistortionApplied);
        } else {
            row.parameters.clone_from(&parameters);
        }
    }

    let current = applied.map(|profile| {
        let (title, subtitle) = card_text(
            profile.camera.fixed_mount,
            &camera_name,
            &profile.lens.model,
            Some(profile.focal.mm),
        );
        let mut card = json!({
            "key": profile.key, "title": title, "subtitle": subtitle,
            "lens": {"maker": profile.lens.maker, "model": profile.lens.model},
            "fixed_mount": profile.camera.fixed_mount,
            "focal_mm": profile.focal.mm,
            "focal_source": profile.focal.source,
        });
        if profile.optics.acknowledged.is_some() {
            card["note"] = json!("Applied assuming the camera did not already correct distortion.");
        }
        card
    });
    let suggestion = match (&detection.detected, state) {
        (Some(detected), State::Detected) => {
            let (title, subtitle) =
                card_text(detection.fixed, &camera_name, &detected.title, focal);
            let mut card = object(json!({
                "key": detected.key, "title": title, "subtitle": subtitle, "label": "Apply",
                "eligible": detected.eligible, "reasons": detected.reasons,
                "match": detected.matched, "focal_range": detected.focal_range,
                "crop_factor": detected.crop_factor, "model": detected.model,
            }));
            if !parameters.is_empty() {
                card.insert("parameters".into(), Value::Object(parameters.clone()));
                card.insert("note".into(), json!(unknown_note));
            }
            Some(Value::Object(card))
        }
        _ => None,
    };

    let lens_name = identity.lens_model.as_deref().map(str::trim);
    let mut notice_value = match state {
        State::Applied => None,
        State::AlreadyCorrected => Some(notice(
            "warning",
            "This photo's source already corrects lens distortion, so a profile would correct it twice.",
        )),
        State::CameraNotFound => Some(notice(
            "warning",
            if identity.make.is_none() && identity.model.is_none() {
                "This photo does not name its camera, so no lens profile can match it.".to_owned()
            } else {
                format!("{camera_name} is not in the lens database.")
            },
        )),
        State::Detected => detection
            .detected
            .as_ref()
            .and_then(|detected| detected_notice(detected, &detection.photo, focal)),
        State::LensNotFound => Some(notice(
            "warning",
            match (detection.candidates.first(), detection.fixed, lens_name) {
                (Some(candidate), _, _) => format!(
                    "The database's profile for {} does not fit this camera's sensor or frame.",
                    candidate.title
                ),
                (None, true, _) => "No profile matches this camera's built-in lens.".to_owned(),
                (None, false, None | Some("")) => {
                    "This photo does not name its lens. Search for it by name.".to_owned()
                }
                (None, false, Some(lens)) => format!("No profile matches {lens}."),
            },
        )),
    };
    let searched = !resolve::normalize(request.text).is_empty();
    let search_empty = search && searched && page.total == 0;
    if search_empty {
        notice_value = Some(notice(
            "warning",
            format!("No compatible profile matches “{}”.", request.text.trim()),
        ));
    }
    let reportable = identity.make.is_some() || identity.model.is_some();
    let report = (reportable
        && (search_empty || matches!(state, State::CameraNotFound | State::LensNotFound)))
    .then(|| {
        report::report(&report::Facts {
            camera_name: &camera_name,
            fixed: detection.fixed,
            optics,
            focal_mm: focal,
            crop_factor: detection.camera.map(|c| c.crop_factor),
            reasons: &reasons,
        })
    });

    let mut visible_shared = Vec::new();
    if !unselectable
        && (input.focal_override.is_some()
            || input.focal_mm.is_none()
            || detection.reasons.contains(&Reason::FocalOutOfRange))
    {
        visible_shared.push("focal");
    }
    let summary = format!(
        "In camera: distortion {} · lateral CA {} · shading {}",
        status_word(ledger.distortion.status),
        status_word(ledger.lateral_ca.status),
        status_word(ledger.shading.status),
    );
    let mut status = object(json!({
        "state": state,
        "summary": summary,
        "visible_shared": visible_shared,
        "search": search,
        "camera": detection.camera.map(|c| json!({"maker": c.maker, "model": c.model})),
        "camera_name": camera_name,
        "product": products::product(identity.make.as_deref(), identity.model.as_deref()),
        "fixed_mount": detection.camera.map(|_| detection.fixed),
        "exif": {"make": identity.make, "model": identity.model,
                 "lens_make": identity.lens_make, "lens_model": identity.lens_model},
        "lens_model": identity.lens_model,
        "focal_mm": focal,
        "focal_source": if input.focal_override.is_some() {
            Some(FocalSource::Override)
        } else {
            input.focal_mm.map(|_| FocalSource::Exif)
        },
        "crop_factor": detection.camera.map(|c| c.crop_factor),
        "distortion": ledger.distortion,
        "lateral_ca": ledger.lateral_ca,
        "shading": ledger.shading,
        "reasons": reasons,
        "detected": detection.detected,
    }));
    for (name, value) in [
        ("current", current),
        ("suggestion", suggestion),
        ("notice", notice_value),
        ("report", report),
    ] {
        if let Some(value) = value {
            status.insert(name.into(), value);
        }
    }
    json!({"status": status, "rows": page.rows, "page": page.page, "pages": page.pages,
           "total": page.total})
}

#[cfg(test)]
#[path = "answer_tests.rs"]
mod answer_tests;

/// What holds a detected profile back, said plainly; nothing when it applies as it stands.
fn detected_notice(detected: &Candidate, photo: &[Reason], focal: Option<f64>) -> Option<Value> {
    let has = |reason| detected.reasons.contains(&reason) || photo.contains(&reason);
    if has(Reason::CropModeMismatch) {
        Some(notice(
            "warning",
            "This photo was taken in a crop mode its camera's profiles do not cover.",
        ))
    } else if has(Reason::UnsupportedStage) {
        Some(notice(
            "warning",
            "This photo is too small for a lens profile.",
        ))
    } else if has(Reason::FocalMissing) {
        Some(notice(
            "warning",
            "This photo does not record its focal length. Enter it to apply the profile.",
        ))
    } else if has(Reason::FocalOutOfRange) {
        let [min, max] = detected.focal_range;
        Some(notice(
            "warning",
            format!(
                "This photo reports {}, outside the profile's calibrated {min}–{max} mm. Enter the focal length to apply it.",
                focal.map_or_else(|| "no focal length".to_owned(), millimetres)
            ),
        ))
    } else {
        None
    }
}
