//! Pure version-1 profile matching and focal interpolation. No catalog, job, network or I/O.
use super::{
    index::{Calibration, DistortionModel, IndexCamera, IndexLens, LensIndex},
    payload::{Camera, FocalSource, Lens},
};
use crate::{Error, modules::Stage};
use serde::Serialize;

#[derive(Clone, Debug)]
pub(crate) struct ResolveInput {
    pub make: String,
    pub model: String,
    pub lens_model: Option<String>,
    pub focal_mm: Option<f64>,
    pub focal_35mm: Option<u16>,
    pub focal_override: Option<f64>,
    pub stage: Stage,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Match {
    LensModel,
    CameraMount,
    Search,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Reason {
    CameraNotInDatabase,
    CalibrationSensorSmaller,
    CropModeMismatch,
    AspectMismatch,
    FocalMissing,
    FocalOutOfRange,
    IncompatibleMount,
    UnsupportedStage,
    EmbeddedDistortionApplied,
    AssumeUncorrectedRequired,
    /// Status only: the camera is in the database but no profile matches its lens identity.
    LensNotInDatabase,
}
impl Reason {
    /// A condition of the photo rather than of one profile: it holds for every profile alike, so
    /// it is reported in status and gates selection but never hides a compatible profile from a
    /// search. Every other reason means the profile does not fit this camera, sensor, frame or
    /// focal length, and such a profile is never listed.
    pub(crate) fn photo_level(self) -> bool {
        matches!(
            self,
            Self::CameraNotInDatabase
                | Self::FocalMissing
                | Self::CropModeMismatch
                | Self::UnsupportedStage
                | Self::EmbeddedDistortionApplied
                | Self::AssumeUncorrectedRequired
                | Self::LensNotInDatabase
        )
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CameraNotInDatabase => "camera-not-in-database",
            Self::CalibrationSensorSmaller => "calibration-sensor-smaller",
            Self::CropModeMismatch => "crop-mode-mismatch",
            Self::AspectMismatch => "aspect-mismatch",
            Self::FocalMissing => "focal-missing",
            Self::EmbeddedDistortionApplied => "embedded-distortion-applied",
            Self::AssumeUncorrectedRequired => "assume-uncorrected-required",
            Self::FocalOutOfRange => "focal-out-of-range",
            Self::IncompatibleMount => "incompatible-mount",
            Self::UnsupportedStage => "unsupported-stage",
            Self::LensNotInDatabase => "lens-not-in-database",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Candidate {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub focal_range: [f64; 2],
    pub crop_factor: f64,
    pub model: DistortionModel,
    #[serde(rename = "match")]
    pub matched: Match,
    pub eligible: bool,
    pub reasons: Vec<Reason>,
    /// Values a selection of this row sends beside its key: the query-choice vocabulary's
    /// per-choice parameters.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub parameters: serde_json::Map<String, serde_json::Value>,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Page {
    pub rows: Vec<Candidate>,
    pub page: usize,
    pub pages: usize,
    pub total: usize,
}
#[derive(Clone, Debug)]
pub(crate) struct Resolution {
    pub key: String,
    pub record_sha256: String,
    pub camera: Camera,
    pub lens: Lens,
    pub model: DistortionModel,
    pub terms: [f64; 3],
    pub focal_mm: f64,
    pub focal_source: FocalSource,
    pub unit_scale: f64,
    pub resolved_long: u32,
    pub resolved_short: u32,
}

/// Lensfun's _lf_strcmp uses Unicode lowercase and collapses whitespace, without fuzzy aliases.
pub(crate) fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}
fn cameras<'a>(index: &'a LensIndex, input: &ResolveInput) -> Vec<&'a IndexCamera> {
    let maker = normalize(&input.make);
    let model = normalize(&input.model);
    index
        .cameras
        .iter()
        .filter(|c| normalize(&c.maker) == maker && normalize(&c.model) == model)
        .collect()
}
fn fixed(index: &LensIndex, camera: &IndexCamera) -> bool {
    index
        .mounts
        .iter()
        .find(|m| m.name == camera.mount)
        .map_or_else(
            || {
                camera
                    .mount
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_lowercase)
            },
            |m| m.fixed,
        )
}
fn compatible(index: &LensIndex, camera: &IndexCamera, lens: &IndexLens) -> bool {
    lens.mounts.contains(&camera.mount)
        || (!fixed(index, camera)
            && index
                .mounts
                .iter()
                .find(|m| m.name == camera.mount)
                .is_some_and(|m| m.compat.iter().any(|n| lens.mounts.contains(n))))
}
fn focal(input: &ResolveInput) -> Option<(f64, FocalSource)> {
    input
        .focal_override
        .map(|f| (f, FocalSource::Override))
        .or_else(|| input.focal_mm.map(|f| (f, FocalSource::Exif)))
}
fn range(lens: &IndexLens) -> [f64; 2] {
    [
        lens.calibrations.first().map_or(0.0, |c| c.focal_mm),
        lens.calibrations.last().map_or(0.0, |c| c.focal_mm),
    ]
}
fn calibration_focal(calibrations: &[Calibration], focal: f64) -> Result<f64, Reason> {
    let first = calibrations
        .first()
        .ok_or(Reason::FocalOutOfRange)?
        .focal_mm;
    let last = calibrations.last().unwrap().focal_mm;
    let minimum = if calibrations.len() == 1 {
        first * 0.99
    } else {
        first / 1.01
    };
    if !focal.is_finite() || focal <= 0.0 || focal < minimum || focal > last * 1.01 {
        return Err(Reason::FocalOutOfRange);
    }
    Ok(focal.clamp(first, last))
}
/// The photo's own conditions against one camera record, the same for every profile.
fn photo_reasons(camera: &IndexCamera, input: &ResolveInput) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if let (Some(f), Some(f35)) = (input.focal_mm, input.focal_35mm)
        && (!f.is_finite() || f <= 0.0 || (f35 as f64 / f / camera.crop_factor - 1.0).abs() > 0.05)
    {
        reasons.push(Reason::CropModeMismatch);
    }
    if input.stage.width.min(input.stage.height) < 2 {
        reasons.push(Reason::UnsupportedStage);
    }
    if focal(input).is_none() {
        reasons.push(Reason::FocalMissing);
    }
    reasons
}
fn reasons(
    index: &LensIndex,
    camera: &IndexCamera,
    lens: &IndexLens,
    input: &ResolveInput,
) -> Vec<Reason> {
    let mut reasons = photo_reasons(camera, input);
    if !compatible(index, camera, lens) {
        reasons.push(Reason::IncompatibleMount);
    }
    if camera.crop_factor < 0.96 * lens.crop_factor {
        reasons.push(Reason::CalibrationSensorSmaller);
    }
    let long = input.stage.width.max(input.stage.height);
    let short = input.stage.width.min(input.stage.height);
    if short >= 2 && ((long as f64 / short as f64) / lens.aspect_ratio - 1.0).abs() > 0.03 {
        reasons.push(Reason::AspectMismatch);
    }
    if let Some((f, _)) = focal(input)
        && calibration_focal(&lens.calibrations, f).is_err()
    {
        reasons.push(Reason::FocalOutOfRange);
    }
    reasons
}
fn candidate(
    index: &LensIndex,
    lens: &IndexLens,
    input: &ResolveInput,
    matched: Match,
    cameras: &[&IndexCamera],
) -> Candidate {
    // The matching camera record the profile fits best: none of its reasons when one fits, else
    // the fewest profile-level ones, so a compatible profile reports only the photo's conditions.
    let mut refusals = cameras
        .iter()
        .map(|camera| reasons(index, camera, lens, input))
        .min_by_key(|current| {
            (
                current.iter().filter(|r| !r.photo_level()).count(),
                current.len(),
            )
        })
        .unwrap_or_else(|| vec![Reason::CameraNotInDatabase]);
    refusals.sort();
    refusals.dedup();
    let eligible = refusals.is_empty();
    Candidate {
        key: lens.key.clone(),
        title: lens.models[0].clone(),
        subtitle: format!(
            "{} · crop {} · {}–{} mm",
            lens.maker,
            lens.crop_factor,
            range(lens)[0],
            range(lens)[1]
        ),
        focal_range: range(lens),
        crop_factor: lens.crop_factor,
        model: lens.model,
        matched,
        eligible,
        reasons: refusals,
        parameters: serde_json::Map::new(),
    }
}
fn identity_lenses<'a>(
    index: &'a LensIndex,
    input: &ResolveInput,
    cameras: &[&IndexCamera],
) -> Vec<(&'a IndexLens, Match)> {
    let model = input.lens_model.as_deref().map(normalize);
    let mut result = Vec::new();
    for lens in &index.lenses {
        let matched = cameras.iter().find_map(|camera| {
            if fixed(index, camera) && lens.mounts.contains(&camera.mount) {
                Some(Match::CameraMount)
            } else if compatible(index, camera, lens)
                && model
                    .as_ref()
                    .is_some_and(|model| lens.models.iter().any(|alias| normalize(alias) == *model))
            {
                Some(Match::LensModel)
            } else {
                None
            }
        });
        if let Some(matched) = matched {
            result.push((lens, matched));
        }
    }
    result.sort_by(|(a, _), (b, _)| a.models[0].cmp(&b.models[0]).then(a.key.cmp(&b.key)));
    result
}
pub(crate) fn candidates(index: &LensIndex, input: &ResolveInput) -> Vec<Candidate> {
    let cameras = cameras(index, input);
    identity_lenses(index, input, &cameras)
        .into_iter()
        .map(|(lens, matched)| candidate(index, lens, input, matched, &cameras))
        .collect()
}
/// Whether `lens` fits at least one matching camera record apart from the photo's own conditions.
fn fits(
    index: &LensIndex,
    lens: &IndexLens,
    input: &ResolveInput,
    cameras: &[&IndexCamera],
) -> bool {
    cameras.iter().any(|camera| {
        reasons(index, camera, lens, input)
            .iter()
            .all(|reason| reason.photo_level())
    })
}
/// Rows per page of a search.
pub(crate) const PAGE_ROWS: usize = 50;
/// The profiles whose maker or model contains `text`, restricted to those that fit this photo's
/// camera, whose every remaining reason is the photo's own ([`Reason::photo_level`]): identity
/// matches first, then by model and key. Empty text
/// lists nothing, and neither does a camera the database does not hold, since no profile could fit
/// it. A profile that does not fit is never listed; `edit.select-lens-profile` still refuses its
/// key with its reasons.
pub(crate) fn search(index: &LensIndex, input: &ResolveInput, text: &str, page: usize) -> Page {
    let needle = normalize(text);
    let cameras = cameras(index, input);
    if needle.is_empty() || cameras.is_empty() {
        return Page {
            rows: Vec::new(),
            page,
            pages: 1,
            total: 0,
        };
    }
    let identity: std::collections::HashMap<_, _> = identity_lenses(index, input, &cameras)
        .into_iter()
        .map(|(lens, matched)| (lens.key.as_str(), matched))
        .collect();
    let mut rows: Vec<_> = index
        .lenses
        .iter()
        .filter(|lens| {
            normalize(&lens.maker).contains(&needle)
                || lens.models.iter().any(|m| normalize(m).contains(&needle))
        })
        .filter(|lens| fits(index, lens, input, &cameras))
        .map(|lens| {
            let matched = identity
                .get(lens.key.as_str())
                .copied()
                .unwrap_or(Match::Search);
            (lens, matched)
        })
        .collect();
    rows.sort_by(|(a, am), (b, bm)| {
        (*am == Match::Search)
            .cmp(&(*bm == Match::Search))
            .then(a.models[0].cmp(&b.models[0]))
            .then(a.key.cmp(&b.key))
    });
    let total = rows.len();
    // Only visible rows allocate response strings. Every text match runs the fit arithmetic,
    // a few comparisons per matching camera record, with no allocation beyond its reason list.
    Page {
        rows: rows
            .into_iter()
            .skip(page.saturating_mul(PAGE_ROWS))
            .take(PAGE_ROWS)
            .map(|(lens, matched)| candidate(index, lens, input, matched, &cameras))
            .collect(),
        page,
        pages: total.div_ceil(PAGE_ROWS).max(1),
        total,
    }
}

/// What this photo's own identity says about the database, without a search.
#[derive(Clone, Debug)]
pub(crate) struct Detection<'a> {
    /// The first matching camera record, which names the camera and its crop factor.
    pub camera: Option<&'a IndexCamera>,
    /// Whether the matched camera has a fixed lens, whose profiles its mount names.
    pub fixed: bool,
    /// The profiles the photo's identity names, in the documented order.
    pub candidates: Vec<Candidate>,
    /// The detected profile: the first eligible candidate, else the first held back only by the
    /// photo's own conditions or its focal length, which a person can supply.
    pub detected: Option<Candidate>,
    /// Why nothing could be applied as it stands: the detected profile's reasons, else every
    /// candidate's, else that the camera or its lens is not in the database.
    pub reasons: Vec<Reason>,
    /// The photo's own conditions against the camera record it fits best, which hold for any
    /// profile a search could find.
    pub photo: Vec<Reason>,
}
pub(crate) fn detect<'a>(index: &'a LensIndex, input: &ResolveInput) -> Detection<'a> {
    let matched = cameras(index, input);
    let photo = matched
        .iter()
        .map(|camera| photo_reasons(camera, input))
        .min_by_key(Vec::len)
        .unwrap_or_default();
    let candidates = candidates(index, input);
    let detected = candidates.iter().find(|c| c.eligible).cloned().or_else(|| {
        candidates
            .iter()
            .find(|c| {
                c.reasons
                    .iter()
                    .all(|r| r.photo_level() || *r == Reason::FocalOutOfRange)
            })
            .cloned()
    });
    let mut reasons = if matched.is_empty() {
        vec![Reason::CameraNotInDatabase]
    } else if let Some(detected) = &detected {
        detected.reasons.clone()
    } else if candidates.is_empty() {
        vec![Reason::LensNotInDatabase]
    } else {
        candidates.iter().flat_map(|c| c.reasons.clone()).collect()
    };
    reasons.sort();
    reasons.dedup();
    Detection {
        camera: matched.first().copied(),
        fixed: matched.first().is_some_and(|camera| fixed(index, camera)),
        candidates,
        detected,
        reasons,
        photo,
    }
}

/// Independently transcribed from lens.cpp:870–943 and auxfun.cpp:441–462 at v0.3.4.
/// The upstream spline is ordered from above to below; every term is scaled by focal first.
pub(crate) fn interpolate(calibrations: &[Calibration], focal: f64) -> Result<[f64; 3], Reason> {
    let focal = calibration_focal(calibrations, focal)?;
    if let Some(exact) = calibrations.iter().find(|c| c.focal_mm == focal) {
        return Ok(exact.terms);
    }
    let above = calibrations
        .iter()
        .position(|c| c.focal_mm > focal)
        .ok_or(Reason::FocalOutOfRange)?;
    let high = &calibrations[above];
    let low = &calibrations[above - 1];
    let next_high = calibrations.get(above + 1);
    let next_low = above.checked_sub(2).and_then(|i| calibrations.get(i));
    let t = (focal - high.focal_mm) / (low.focal_mm - high.focal_mm);
    let t2 = t * t;
    let t3 = t2 * t;
    Ok(std::array::from_fn(|i| {
        let y2 = high.terms[i] * high.focal_mm;
        let y3 = low.terms[i] * low.focal_mm;
        let tangent2 = next_high.map_or(y3 - y2, |c| (y3 - c.terms[i] * c.focal_mm) * 0.5);
        let tangent3 = next_low.map_or(y3 - y2, |c| (c.terms[i] * c.focal_mm - y2) * 0.5);
        ((2.0 * t3 - 3.0 * t2 + 1.0) * y2
            + (t3 - 2.0 * t2 + t) * tangent2
            + (-2.0 * t3 + 3.0 * t2) * y3
            + (t3 - t2) * tangent3)
            / focal
    }))
}
pub(crate) fn resolve(
    index: &LensIndex,
    key: &str,
    input: &ResolveInput,
) -> Result<Resolution, Error> {
    let lens = index
        .lenses
        .iter()
        .find(|l| l.key == key)
        .ok_or_else(|| Error::validation(format!("unknown lens profile {key}")))?;
    let cameras = cameras(index, input);
    let row = candidate(index, lens, input, Match::Search, &cameras);
    if !row.eligible {
        if row.reasons.contains(&Reason::FocalOutOfRange) {
            let f = focal(input).map_or(0.0, |f| f.0);
            let [min, max] = range(lens);
            return Err(Error::validation(format!(
                "focal length {f} mm is outside the profile's calibrated {min}–{max} mm"
            ))
            .with_data(
                serde_json::json!({"reason":"focal-out-of-range","focal":f,"min":min,"max":max}),
            ));
        }
        let reason = row.reasons[0];
        return Err(Error::unsupported_input(format!(
            "profile {key} is not supported: {}",
            reason.label()
        ))
        .with_data(serde_json::json!({"reason":reason,"reasons":row.reasons})));
    }
    let camera = cameras
        .into_iter()
        .find(|c| reasons(index, c, lens, input).is_empty())
        .expect("eligible camera");
    let (focal_mm, focal_source) = focal(input).expect("eligible focal");
    let terms = interpolate(&lens.calibrations, focal_mm)
        .map_err(|_| Error::validation("Invalid focal calibration"))?;
    let resolved_long = input.stage.width.max(input.stage.height);
    let resolved_short = input.stage.width.min(input.stage.height);
    let long = resolved_long as f64;
    let short = resolved_short as f64;
    let aspect = (long - 1.0) / (short - 1.0);
    let crop_correction = (lens.aspect_ratio * lens.aspect_ratio + 1.0).sqrt()
        / (aspect * aspect + 1.0).sqrt()
        * lens.crop_factor
        / camera.crop_factor;
    let unit_scale = crop_correction * short / (short - 1.0);
    Ok(Resolution {
        key: lens.key.clone(),
        record_sha256: lens.record_sha256.clone(),
        camera: Camera {
            maker: camera.maker.clone(),
            model: camera.model.clone(),
            crop_factor: camera.crop_factor,
            fixed_mount: fixed(index, camera),
        },
        lens: Lens {
            maker: lens.maker.clone(),
            model: lens.models[0].clone(),
            crop_factor: lens.crop_factor,
            aspect_ratio: lens.aspect_ratio,
        },
        model: lens.model,
        terms,
        focal_mm,
        focal_source,
        unit_scale,
        resolved_long,
        resolved_short,
    })
}
#[cfg(test)]
#[path = "resolve_tests.rs"]
mod resolve_tests;
