//! A frozen resolved profile is sufficient for evaluation. Validation never reads the index.
use super::{index::DistortionModel, pinned, resolve::Resolution};
use crate::{Error, SourceOptics};
use luxforge_raw::OpticalStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const INTERPRETATION: &str = "lensfun-v1-distortion-edge-1";
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Payload {
    pub profile: Option<Profile>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    pub key: String,
    pub interpretation: String,
    pub database: Database,
    pub camera: Camera,
    pub lens: Lens,
    pub focal: Focal,
    pub model: DistortionModel,
    pub terms: [f64; 3],
    pub normalization: Normalization,
    pub optics: Optics,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Database {
    pub release: String,
    pub commit: String,
    pub index_sha256: String,
    pub record_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Camera {
    pub maker: String,
    pub model: String,
    pub crop_factor: f64,
    pub fixed_mount: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lens {
    pub maker: String,
    pub model: String,
    pub crop_factor: f64,
    pub aspect_ratio: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FocalSource {
    Exif,
    Override,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Focal {
    pub mm: f64,
    pub source: FocalSource,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Normalization {
    pub unit_scale: f64,
    pub resolved_long: u32,
    pub resolved_short: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Acknowledgement {
    AssumeUncorrected,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Optics {
    pub source_interpretation: String,
    pub distortion: OpticalStatus,
    pub acknowledged: Option<Acknowledgement>,
}

fn valid_hash(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}
fn name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

/// A frozen Poly3 profile of `k1` resolved for a `stage`, built without the index: the payload a
/// detected profile's Apply would commit, for the qualification tests that draw a lens warp.
#[cfg(feature = "qualification")]
pub(crate) fn qualification(k1: f64, stage: (u32, u32)) -> Value {
    let (long, short) = (stage.0.max(stage.1), stage.0.min(stage.1));
    let record = "0123456789abcdef".repeat(4);
    let aspect = (f64::from(long) - 1.0) / (f64::from(short) - 1.0);
    let lens_aspect = 1.5_f64;
    let unit_scale = (lens_aspect * lens_aspect + 1.0).sqrt() / (aspect * aspect + 1.0).sqrt()
        * f64::from(short)
        / (f64::from(short) - 1.0);
    let profile = Profile {
        key: format!("lf1-{}", &record[..16]),
        interpretation: INTERPRETATION.into(),
        database: Database {
            release: pinned::RELEASE.into(),
            commit: pinned::COMMIT.into(),
            index_sha256: pinned::INDEX_SHA256.into(),
            record_sha256: record,
        },
        camera: Camera {
            maker: "Luxforge".into(),
            model: "Qualification".into(),
            crop_factor: 1.0,
            fixed_mount: true,
        },
        lens: Lens {
            maker: "Luxforge".into(),
            model: "Qualification lens".into(),
            crop_factor: 1.0,
            aspect_ratio: lens_aspect,
        },
        focal: Focal {
            mm: 24.0,
            source: FocalSource::Exif,
        },
        model: DistortionModel::Poly3,
        terms: [k1, 0.0, 0.0],
        normalization: Normalization {
            unit_scale,
            resolved_long: long,
            resolved_short: short,
        },
        optics: Optics {
            source_interpretation: "qualification".into(),
            distortion: OpticalStatus::KnownUnapplied,
            acknowledged: None,
        },
    };
    profile.validate().expect("a valid frozen profile");
    serde_json::to_value(Payload {
        profile: Some(profile),
    })
    .expect("a payload")
}

pub(crate) fn parse(value: &Value) -> Result<Payload, Error> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| Error::validation(format!("Invalid lens payload: {e}")))?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(Error::validation("Lens payload exceeds 16 KiB"));
    }
    let payload: Payload = serde_json::from_slice(&bytes)
        .map_err(|e| Error::validation(format!("Invalid lens payload: {e}")))?;
    if let Some(profile) = &payload.profile {
        profile.validate()?;
    }
    Ok(payload)
}
impl Profile {
    pub(crate) fn from_resolution(
        resolution: Resolution,
        source: &SourceOptics,
        assume_uncorrected: bool,
    ) -> Result<Self, Error> {
        let optics = Optics {
            source_interpretation: source.ledger.interpretation.clone(),
            distortion: source.ledger.distortion.status,
            acknowledged: assume_uncorrected.then_some(Acknowledgement::AssumeUncorrected),
        };
        if optics.distortion == OpticalStatus::Applied {
            return Err(Error::incompatible(format!("this photo's source already corrects lens distortion ({}); a profile would correct it twice",source.ledger.distortion.provenance))
                .with_data(serde_json::json!({"reason":"embedded-distortion-applied"})));
        }
        if optics.distortion == OpticalStatus::Unknown && optics.acknowledged.is_none() {
            return Err(Error::incompatible("assume-uncorrected is required: this photo's distortion correction status is unknown")
                .with_data(serde_json::json!({"reason":"assume-uncorrected-required"})));
        }
        let profile = Self {
            key: resolution.key,
            interpretation: INTERPRETATION.into(),
            database: Database {
                release: pinned::RELEASE.into(),
                commit: pinned::COMMIT.into(),
                index_sha256: pinned::INDEX_SHA256.into(),
                record_sha256: resolution.record_sha256,
            },
            camera: resolution.camera,
            lens: resolution.lens,
            focal: Focal {
                mm: resolution.focal_mm,
                source: resolution.focal_source,
            },
            model: resolution.model,
            terms: resolution.terms,
            normalization: Normalization {
                unit_scale: resolution.unit_scale,
                resolved_long: resolution.resolved_long,
                resolved_short: resolution.resolved_short,
            },
            optics,
        };
        profile.validate()?;
        Ok(profile)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let malformed = || Error::validation("Invalid frozen lens profile payload");
        if self.interpretation != INTERPRETATION
            || self.database.release != pinned::RELEASE
            || self.database.commit != pinned::COMMIT
            || self.database.index_sha256 != pinned::INDEX_SHA256
        {
            return Err(Error::incompatible(
                "Unsupported frozen lens profile interpretation or database identity",
            ));
        }
        if !valid_hash(&self.database.record_sha256, 64)
            || self.key != format!("lf1-{}", &self.database.record_sha256[..16])
            || !name(&self.camera.maker)
            || !name(&self.camera.model)
            || !name(&self.lens.maker)
            || !name(&self.lens.model)
            || !positive(self.camera.crop_factor)
            || !positive(self.lens.crop_factor)
            || !positive(self.lens.aspect_ratio)
            || !(0.5..=2000.0).contains(&self.focal.mm)
            || !self.focal.mm.is_finite()
            || self.terms.iter().any(|v| !v.is_finite())
            || !positive(self.normalization.unit_scale)
            || self.normalization.resolved_short < 2
            || self.normalization.resolved_long < self.normalization.resolved_short
            || !name(&self.optics.source_interpretation)
        {
            return Err(malformed());
        }
        match self.model {
            DistortionModel::Poly3 if self.terms[1] != 0.0 || self.terms[2] != 0.0 => {
                return Err(malformed());
            }
            DistortionModel::Poly5 if self.terms[2] != 0.0 => return Err(malformed()),
            _ => {}
        }
        let long = self.normalization.resolved_long as f64;
        let short = self.normalization.resolved_short as f64;
        let aspect = (long - 1.0) / (short - 1.0);
        let expected = (self.lens.aspect_ratio * self.lens.aspect_ratio + 1.0).sqrt()
            / (aspect * aspect + 1.0).sqrt()
            * self.lens.crop_factor
            / self.camera.crop_factor
            * short
            / (short - 1.0);
        if !positive(expected) || (self.normalization.unit_scale / expected - 1.0).abs() > 1e-12 {
            return Err(Error::validation(
                "Frozen lens normalization differs from its recorded stage and optics",
            ));
        }
        Ok(())
    }
}
