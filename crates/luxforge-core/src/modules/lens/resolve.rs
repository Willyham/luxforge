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
}
impl Reason {
    fn label(self) -> &'static str {
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
pub(crate) fn camera_match<'a>(
    index: &'a LensIndex,
    input: &ResolveInput,
) -> Option<&'a IndexCamera> {
    cameras(index, input).into_iter().next()
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
fn reasons(
    index: &LensIndex,
    camera: &IndexCamera,
    lens: &IndexLens,
    input: &ResolveInput,
) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if !compatible(index, camera, lens) {
        reasons.push(Reason::IncompatibleMount);
    }
    if camera.crop_factor < 0.96 * lens.crop_factor {
        reasons.push(Reason::CalibrationSensorSmaller);
    }
    if let (Some(f), Some(f35)) = (input.focal_mm, input.focal_35mm)
        && (!f.is_finite() || f <= 0.0 || (f35 as f64 / f / camera.crop_factor - 1.0).abs() > 0.05)
    {
        reasons.push(Reason::CropModeMismatch);
    }
    let long = input.stage.width.max(input.stage.height);
    let short = input.stage.width.min(input.stage.height);
    if short < 2 {
        reasons.push(Reason::UnsupportedStage);
    } else if ((long as f64 / short as f64) / lens.aspect_ratio - 1.0).abs() > 0.03 {
        reasons.push(Reason::AspectMismatch);
    }
    match focal(input) {
        None => reasons.push(Reason::FocalMissing),
        Some((f, _)) => {
            if calibration_focal(&lens.calibrations, f).is_err() {
                reasons.push(Reason::FocalOutOfRange);
            }
        }
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
    let mut refusals = Vec::new();
    let eligible = cameras.iter().any(|camera| {
        let current = reasons(index, camera, lens, input);
        if current.is_empty() {
            true
        } else {
            refusals.extend(current);
            false
        }
    });
    if cameras.is_empty() {
        refusals.push(Reason::CameraNotInDatabase);
    }
    if eligible {
        refusals.clear();
    }
    refusals.sort();
    refusals.dedup();
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
pub(crate) fn search(index: &LensIndex, input: &ResolveInput, text: &str, page: usize) -> Page {
    let cameras = cameras(index, input);
    let mut rows = identity_lenses(index, input, &cameras);
    let existing: std::collections::HashSet<_> =
        rows.iter().map(|(lens, _)| lens.key.as_str()).collect();
    let needle = normalize(text);
    let mut matches: Vec<_> = index
        .lenses
        .iter()
        .filter(|lens| {
            !existing.contains(lens.key.as_str())
                && (needle.is_empty()
                    || normalize(&lens.maker).contains(&needle)
                    || lens.models.iter().any(|m| normalize(m).contains(&needle)))
        })
        .map(|lens| (lens, Match::Search))
        .collect();
    matches.sort_by(|(a, _), (b, _)| a.models[0].cmp(&b.models[0]).then(a.key.cmp(&b.key)));
    rows.extend(matches);
    let total = rows.len();
    let pages = total.div_ceil(50);
    // Only visible rows allocate response strings and run eligibility arithmetic. The query
    // computes camera identity once, rather than repeating its scan for every database lens.
    Page {
        rows: rows
            .into_iter()
            .skip(page.saturating_mul(50))
            .take(50)
            .map(|(lens, matched)| candidate(index, lens, input, matched, &cameras))
            .collect(),
        page,
        pages,
        total,
    }
}
pub(crate) fn status_reasons(index: &LensIndex, input: &ResolveInput) -> Vec<Reason> {
    if cameras(index, input).is_empty() {
        vec![Reason::CameraNotInDatabase]
    } else {
        let mut reasons: Vec<_> = candidates(index, input)
            .into_iter()
            .flat_map(|c| c.reasons)
            .collect();
        reasons.sort();
        reasons.dedup();
        reasons
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
