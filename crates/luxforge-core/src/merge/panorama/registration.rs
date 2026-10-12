//! Registration of 2–32 panorama frames under the rotating-camera model ([`super::camera`]), from
//! each frame's [`FrameFeatures`] alone (`docs/design/panorama-merge.md`, "Registration").
//!
//! 1. **Texture.** A frame with fewer than [`MIN_FRAME_FEATURES`] features is refused
//!    ([`Refusal::LowTexture`]).
//! 2. **Matching.** Every pair, at most [`MAX_PAIRS`], is matched ([`super::matching`]) and fitted
//!    by RANSAC with a homography within [`HOMOGRAPHY_THRESHOLD_PX`] proxy pixels; the fit is
//!    verified in the manner of Brown and Lowe. Frames the verified pairs do not connect to the
//!    largest group are refused ([`Refusal::Disconnected`]).
//! 3. **Focal length.** The initial focal length is the recorded one ([`FocalPrior`]) or else the
//!    median of what the verified homographies imply, or the frame's diagonal when none implies
//!    one. Around it, a search over [`FOCAL_STEP`] steps (at most [`MAX_FOCAL_CANDIDATES`]) keeps
//!    the focal length under which the rotations fitted to every verified pair's homography
//!    inliers (at most [`FOCAL_SAMPLES`] a pair) agree with the most of them within
//!    [`ROTATION_THRESHOLD_PX`].
//! 4. **Rotations.** Each verified pair's rotation is fitted (Horn) to its homography inliers at
//!    that focal length, its inliers among all its putative matches found within
//!    [`ROTATION_THRESHOLD_PX`], refitted and verified again: a rotation-induced homography. A
//!    maximum spanning tree of the accepted pairs, by inlier count, from the frame with the most
//!    inliers, gives each frame's initial rotation.
//! 5. **Bundle adjustment** ([`super::bundle`]) of every accepted pair's inliers (at most
//!    [`MAX_BUNDLE_MATCHES`] a pair) with Huber's loss at [`HUBER_PX`], at most
//!    [`MAX_BUNDLE_ITERATIONS`] iterations, then every verified pair is classified again under
//!    the adjusted model within [`INLIER_THRESHOLD_PX`] and verified; a frame not yet placed is
//!    placed from a verified pair to a placed frame. At most [`MAX_BUNDLE_ROUNDS`] rounds, until
//!    the inliers settle. The classification after the last adjustment is what the pairs record.
//! 6. **Connectivity.** Frames the accepted pairs leave out of the largest group are refused:
//!    [`Refusal::AmbiguousRegistration`] when a verified homography links them to that group (they
//!    share detail but do not register as one turning camera), else [`Refusal::Disconnected`].
//! 7. **Same view.** Two frames whose corners land within [`SAME_VIEW_SHIFT`] of the diagonal of
//!    each other and that overlap by [`SAME_VIEW_OVERLAP`] both ways are refused
//!    ([`Refusal::SameView`]).
//! 8. **Levelling.** The world's down is the direction most nearly perpendicular to every frame's
//!    right axis (wave correction), regularized by [`LEVELLING_WEIGHT`] towards the frames' own
//!    down, so a column or a pair of nearly parallel frames keeps their recorded up. When that
//!    leaves a frame within [`LEVEL_CHECK_PITCH_DEG`] of the horizon rolled by more than
//!    [`MAX_LEVEL_ROLL_DEG`], or the levelled down is further than that from the frames' mean
//!    down, the frames' recorded orientation contradicts the levelling, and their mean down is
//!    used instead; the frame then rolled furthest is named. The world's yaw 0 is then the centre
//!    of the frames' yaw arc.
//! 9. **Reference.** The requested frame, or the frame whose optical axis is nearest the
//!    panorama's centre (the yaw arc's centre at the middle of the frames' pitch range); frames
//!    within [`REFERENCE_TIE_DEG`] of the nearest tie, broken by capture order.
//!
//! Thresholds in proxy pixels scale by the proxies' box factor to full-resolution pixels, the
//! units of every result. Everything is deterministic: RANSAC draws from a generator seeded by
//! the pair, and every pass over pairs runs each pair independently (on the shared pool) and
//! combines them in pair order. `cancel` is checked before each stage, pair, focal candidate and
//! bundle iteration, and within the matching and RANSAC loops ([`super::matching`]).

use super::bundle::{self, PairObservations};
use super::camera::{Lens, MAX_RADIAL, Rotation, rotation_from};
use super::features::FrameFeatures;
use super::linalg::{self, M3, V3};
use super::matching::{self, HomographyFit, Normalization};
use super::refusal::Refusal;
use crate::{Cancel, Error};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// The fewest frames a panorama registers.
pub const MIN_FRAMES: usize = 2;
/// The most frames a panorama registers.
pub const MAX_FRAMES: usize = 32;
/// The most pairs matched: every pair of [`MAX_FRAMES`].
pub const MAX_PAIRS: usize = MAX_FRAMES * (MAX_FRAMES - 1) / 2;
/// A frame with fewer features is refused as low texture.
pub const MIN_FRAME_FEATURES: usize = 32;
/// The homography RANSAC's inlier threshold, in proxy pixels both ways.
pub const HOMOGRAPHY_THRESHOLD_PX: f64 = 3.0;
/// The initial rotation fits' inlier threshold, in proxy pixels both ways: generous, since the
/// focal length is not yet adjusted.
pub const ROTATION_THRESHOLD_PX: f64 = 3.0;
/// The adjusted model's inlier threshold, in proxy pixels both ways: a match further than this
/// from where the registration carries it is not an inlier.
pub const INLIER_THRESHOLD_PX: f64 = 2.0;
/// Huber's scale in the bundle adjustment, in proxy pixels.
pub const HUBER_PX: f64 = 1.0;
/// The most inliers of one pair the bundle adjustment reads, spread evenly over the pair's
/// inliers in feature order.
pub const MAX_BUNDLE_MATCHES: usize = 150;
/// The most Levenberg–Marquardt iterations of one bundle adjustment.
pub const MAX_BUNDLE_ITERATIONS: usize = 50;
/// The most rounds of adjustment and classification.
pub const MAX_BUNDLE_ROUNDS: usize = 3;
/// The focal search's ratio between neighbouring candidates.
pub const FOCAL_STEP: f64 = 1.01;
/// The most candidate focal lengths searched.
pub const MAX_FOCAL_CANDIDATES: usize = 256;
/// The most homography inliers of one pair each focal candidate reads.
pub const FOCAL_SAMPLES: usize = 128;
/// The search spans the recorded focal length ×/÷ this...
pub const FOCAL_SPAN_RECORDED: f64 = 1.25;
/// ...the homographies' median ×/÷ this...
pub const FOCAL_SPAN_MATCHED: f64 = 1.5;
/// ...and the diagonal ×/÷ this when neither gives one.
pub const FOCAL_SPAN_UNKNOWN: f64 = 3.0;
/// Two frames are the same view when every corner of one lands within this fraction of the
/// frame's diagonal of the other's...
pub const SAME_VIEW_SHIFT: f64 = 0.02;
/// ...and each covers at least this fraction of the other.
pub const SAME_VIEW_OVERLAP: f64 = 0.9;
/// The levelling's pull towards the frames' own down, per frame, beside wave correction's one per
/// frame.
pub const LEVELLING_WEIGHT: f64 = 1e-3;
/// A levelled frame rolled further than this contradicts the levelling...
pub const MAX_LEVEL_ROLL_DEG: f64 = 45.0;
/// ...when its optical axis is within this of the horizon (a frame looking up or down has no
/// meaningful roll).
pub const LEVEL_CHECK_PITCH_DEG: f64 = 60.0;
/// Frames whose distance from the panorama's centre is within this of the nearest tie for the
/// reference.
pub const REFERENCE_TIE_DEG: f64 = 0.1;
/// Frames looking further than this from the horizon do not count towards the yaw arc.
const ARC_PITCH_LIMIT_DEG: f64 = 80.0;
/// The grid each frame is sampled on for the same-view overlap.
const OVERLAP_GRID: usize = 16;

/// The most bytes one registration of `frames` frames holds at once besides their features: every
/// pair's putative matches (two points each) and, while matching, each pair's nearest-neighbour
/// tables; every pair's homography and rotation inliers; the focal search's samples; the bundle
/// adjustment's observations, per-pair blocks and normal equations. Each term takes every pair at
/// its bound ([`super::features::MAX_FEATURES`] features a frame), so the real figure is lower.
pub fn scratch_bytes_for(frames: usize) -> usize {
    use super::features::MAX_FEATURES;
    let pairs = frames * frames.saturating_sub(1) / 2;
    let point = 2 * size_of::<(f64, f64)>();
    let matches = pairs * MAX_FEATURES * point;
    let tables = pairs * MAX_FEATURES * (3 * size_of::<u32>() + 2 * size_of::<u32>());
    let inliers = 2 * pairs * MAX_FEATURES * size_of::<u32>();
    let focal = pairs * FOCAL_SAMPLES * point;
    let bundle = pairs * (MAX_BUNDLE_MATCHES * point + (64 + 8 + 1) * size_of::<f64>());
    let unknowns = 3 * frames + 2;
    let normal = 2 * unknowns * unknowns * size_of::<f64>();
    matches + tables + inliers + focal + bundle + normal
}

/// What the camera recorded about its focal length, to start the search from.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FocalPrior {
    /// The focal length and the sensor's longer side, in millimetres: the frame's longer side
    /// spans the sensor's.
    Sensor { focal_mm: f64, sensor_long_mm: f64 },
    /// The 35 mm equivalent focal length: the frame's diagonal spans 43.27 mm.
    Equivalent35 { focal_mm: f64 },
}

impl FocalPrior {
    /// The focal length in pixels of a `width × height` frame, when the record is usable.
    pub fn focal_px(&self, width: u32, height: u32) -> Option<f64> {
        let (w, h) = (f64::from(width), f64::from(height));
        let f = match *self {
            Self::Sensor {
                focal_mm,
                sensor_long_mm,
            } => focal_mm * w.max(h) / sensor_long_mm,
            Self::Equivalent35 { focal_mm } => {
                focal_mm * (w * w + h * h).sqrt() / 36f64.hypot(24.0)
            }
        };
        (f.is_finite() && f > 0.0).then_some(f)
    }
}

/// Whether the radial term is fitted or fixed (a lens profile already applied fixes it at 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RadialMode {
    #[default]
    Fit,
    Fixed {
        radial: f64,
    },
}

/// The registration's options.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegistrationOptions {
    /// The reference the person named, by request position.
    pub reference: Option<usize>,
    pub radial: RadialMode,
    pub focal: Option<FocalPrior>,
    /// The request positions in capture order (a permutation); request order when absent.
    pub capture_order: Option<Vec<usize>>,
}

/// The registration's stages, reported as each starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrationStage {
    Matching,
    Verifying,
    Focal,
    Bundle,
    Levelling,
}

/// Where the focal length's starting point came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FocalOrigin {
    Recorded,
    Matches,
    Diagonal,
}

/// Where the radial term came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RadialOrigin {
    Fitted,
    Fixed,
}

/// How the reference was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceOrigin {
    Requested,
    Centre,
}

/// How the world's down was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Levelling {
    /// Wave correction.
    Levelled,
    /// The frames' mean down, because levelling contradicted their recorded orientation; `frame`
    /// is the frame rolled furthest from it, by `roll_deg`.
    Contradicted { frame: usize, roll_deg: f64 },
}

/// What became of one pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairStatus {
    /// Too few putative matches, or no homography verified.
    Unmatched,
    /// A verified homography that the rotating camera does not register.
    Inconsistent,
    /// Registered: its inliers verify under the adjusted model.
    Accepted,
}

/// One pair's evidence, under the final model for an accepted or inconsistent pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairEvidence {
    pub a: usize,
    pub b: usize,
    pub status: PairStatus,
    /// Putative matches.
    pub matches: u32,
    /// Putative matches in the model's overlap.
    pub overlap: u32,
    pub inliers: u32,
    /// Inliers among the putative matches in the overlap.
    pub inlier_ratio: f64,
    /// The root mean square of the inliers' transfer residuals both ways, full-resolution pixels.
    pub rms_residual_px: f64,
}

/// One frame's evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameEvidence {
    pub features: u32,
    /// Inliers over its accepted pairs.
    pub inliers: u32,
    pub rms_residual_px: f64,
}

/// Counts that show the bounds held.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistrationDiagnostics {
    pub pairs: usize,
    /// The most RANSAC draws any pair made.
    pub max_ransac_draws: usize,
    pub focal_candidates: usize,
    pub bundle_rounds: usize,
    /// Iterations of each round.
    pub bundle_iterations: Vec<usize>,
    /// The most inliers any adjustment read from one pair.
    pub max_bundle_matches: usize,
}

/// A registered panorama: the shared lens and one rotation a frame in a levelled world whose `+y`
/// is down and whose yaw 0 is the centre of the frames' yaw arc. Full-resolution pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanoramaRegistration {
    pub lens: Lens,
    pub focal_origin: FocalOrigin,
    pub radial_origin: RadialOrigin,
    /// World to camera, by request position.
    pub rotations: Vec<Rotation>,
    pub frames: Vec<FrameEvidence>,
    /// Every pair `a < b`, in order.
    pub pairs: Vec<PairEvidence>,
    pub reference: usize,
    pub reference_origin: ReferenceOrigin,
    pub levelling: Levelling,
    /// Over every accepted pair's inliers both ways.
    pub rms_residual_px: f64,
    pub max_residual_px: f64,
    pub diagnostics: RegistrationDiagnostics,
}

impl PanoramaRegistration {
    /// Where pixel `pixel` of frame `from` lands in frame `to`.
    pub fn transfer(&self, to: usize, from: usize, pixel: (f64, f64)) -> Option<(f64, f64)> {
        self.lens
            .transfer(&self.rotations[to], &self.rotations[from], pixel)
    }

    /// The world direction (unit) pixel `pixel` of `frame` shows.
    pub fn world_ray(&self, frame: usize, pixel: (f64, f64)) -> [f64; 3] {
        self.lens.world_ray(&self.rotations[frame], pixel)
    }

    /// Where a world direction lands in `frame`, if in front of it.
    pub fn world_project(&self, frame: usize, world: [f64; 3]) -> Option<(f64, f64)> {
        self.lens.world_project(&self.rotations[frame], world)
    }
}

/// One pair's working state.
struct PairState {
    a: usize,
    b: usize,
    points: Vec<((f64, f64), (f64, f64))>,
    /// The coarser pyramid level of each match's two features.
    levels: Vec<u8>,
    homography: Option<HomographyFit>,
    verified: bool,
    /// The rotation of `b`'s camera relative to `a`'s, and the inliers under it.
    rotation: Option<(Rotation, Vec<u32>)>,
    overlap: usize,
    rms: f64,
}

/// Register `frames`. See the module documentation.
pub fn register(
    frames: &[FrameFeatures],
    options: &RegistrationOptions,
    cancel: &Cancel,
) -> Result<PanoramaRegistration, Error> {
    register_with_progress(frames, options, cancel, &mut |_| {})
}

/// [`register`], calling `progress` as each stage starts.
pub fn register_with_progress(
    frames: &[FrameFeatures],
    options: &RegistrationOptions,
    cancel: &Cancel,
    progress: &mut dyn FnMut(RegistrationStage),
) -> Result<PanoramaRegistration, Error> {
    let n = frames.len();
    if !(MIN_FRAMES..=MAX_FRAMES).contains(&n) {
        return Err(Error::validation(format!(
            "a panorama registers {MIN_FRAMES} to {MAX_FRAMES} frames, not {n}"
        )));
    }
    let (width, height, factor) = (frames[0].width, frames[0].height, frames[0].factor);
    if frames
        .iter()
        .any(|f| (f.width, f.height, f.factor) != (width, height, factor))
    {
        return Err(Error::validation(
            "panorama frames must share one size and proxy scale",
        ));
    }
    if options.reference.is_some_and(|r| r >= n) {
        return Err(Error::validation(
            "the requested reference is not one of the frames",
        ));
    }
    let capture = match &options.capture_order {
        Some(order) => {
            let mut seen = vec![false; n];
            if order.len() != n
                || order
                    .iter()
                    .any(|&i| i >= n || std::mem::replace(&mut seen[i], true))
            {
                return Err(Error::validation(
                    "the capture order must name every frame once",
                ));
            }
            order.clone()
        }
        None => (0..n).collect(),
    };
    let (radial, radial_origin) = match options.radial {
        RadialMode::Fit => (0.0, RadialOrigin::Fitted),
        RadialMode::Fixed { radial } if radial.is_finite() && radial.abs() <= MAX_RADIAL => {
            (radial, RadialOrigin::Fixed)
        }
        RadialMode::Fixed { .. } => {
            return Err(Error::validation(format!(
                "a fixed radial term is at most {MAX_RADIAL} either way"
            )));
        }
    };
    let px = f64::from(factor);

    // 1. Texture.
    let low: Vec<usize> = (0..n)
        .filter(|&i| frames[i].len() < MIN_FRAME_FEATURES)
        .collect();
    if !low.is_empty() {
        return Err(Refusal::LowTexture { frames: low }.into());
    }

    // 2. Matching and verification.
    cancel.check()?;
    progress(RegistrationStage::Matching);
    cancel.check()?;
    let pair_list: Vec<(usize, usize)> = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .collect();
    type Matched = (Vec<((f64, f64), (f64, f64))>, Vec<u8>);
    let matched: Vec<Matched> = pair_list
        .par_iter()
        .map(|&(a, b)| {
            cancel.check()?;
            let matches = matching::putative_matches(&frames[a], &frames[b], cancel)?;
            Ok(matches
                .iter()
                .map(|&(i, j)| {
                    let (p, q) = (
                        &frames[a].features[i as usize],
                        &frames[b].features[j as usize],
                    );
                    (((p.x, p.y), (q.x, q.y)), p.level.max(q.level))
                })
                .unzip())
        })
        .collect::<Result<_, Error>>()?;
    cancel.check()?;
    progress(RegistrationStage::Verifying);
    cancel.check()?;
    let norm = Normalization::of(width, height);
    let lens_bounds = Lens {
        width,
        height,
        focal_px: 1.0,
        radial: 0.0,
    };
    let mut pairs: Vec<PairState> = pair_list
        .par_iter()
        .zip(matched.into_par_iter())
        .map(|(&(a, b), (points, levels))| {
            cancel.check()?;
            let seed = 0x5A17_0000_0000_0000u64 ^ ((a as u64) << 32) ^ b as u64;
            let homography = matching::fit_homography(
                &points,
                norm,
                HOMOGRAPHY_THRESHOLD_PX * px,
                seed,
                cancel,
            )?;
            let (verified, overlap) = match &homography {
                Some(fit) => {
                    let overlap = matching::invert(&fit.h).map_or(0, |inverse| {
                        points
                            .iter()
                            .filter(|&&(p, q)| {
                                matching::map(&fit.h, p).is_some_and(|m| lens_bounds.contains(m))
                                    && matching::map(&inverse, q)
                                        .is_some_and(|m| lens_bounds.contains(m))
                            })
                            .count()
                    });
                    (matching::verified(fit.inliers.len(), overlap), overlap)
                }
                None => (false, 0),
            };
            Ok(PairState {
                a,
                b,
                points,
                levels,
                homography,
                verified,
                rotation: None,
                overlap,
                rms: 0.0,
            })
        })
        .collect::<Result<_, Error>>()?;
    let mut diagnostics = RegistrationDiagnostics {
        pairs: pairs.len(),
        max_ransac_draws: pairs
            .iter()
            .filter_map(|p| p.homography.as_ref().map(|h| h.draws))
            .max()
            .unwrap_or(0),
        ..Default::default()
    };
    let shared = components(n, pairs.iter().filter(|p| p.verified).map(|p| (p.a, p.b)));
    let main = largest(&shared);
    let apart: Vec<usize> = (0..n).filter(|&i| shared[i] != main).collect();
    if !apart.is_empty() {
        return Err(Refusal::Disconnected { frames: apart }.into());
    }

    // 3. Focal length.
    cancel.check()?;
    progress(RegistrationStage::Focal);
    cancel.check()?;
    let (centre, span, focal_origin) = match options
        .focal
        .and_then(|prior| prior.focal_px(width, height))
    {
        Some(f) => (f, FOCAL_SPAN_RECORDED, FocalOrigin::Recorded),
        None => {
            let (cx, cy) = lens_bounds.centre();
            let mut estimates: Vec<f64> = pairs
                .iter()
                .filter(|p| p.verified)
                .filter_map(|p| {
                    let h = matching::centred(&p.homography.as_ref()?.h, cx, cy);
                    match matching::focals_from_homography(&h) {
                        (Some(f0), Some(f1)) => Some((f0 * f1).sqrt()),
                        (Some(f), None) | (None, Some(f)) => Some(f),
                        (None, None) => None,
                    }
                })
                .filter(|f| {
                    *f > 0.05 * lens_bounds.diagonal() && *f < 50.0 * lens_bounds.diagonal()
                })
                .collect();
            estimates.sort_by(f64::total_cmp);
            match estimates.len() {
                0 => (
                    lens_bounds.diagonal(),
                    FOCAL_SPAN_UNKNOWN,
                    FocalOrigin::Diagonal,
                ),
                len => (estimates[len / 2], FOCAL_SPAN_MATCHED, FocalOrigin::Matches),
            }
        }
    };
    let steps =
        ((span.ln() / FOCAL_STEP.ln()).ceil() as i64).min((MAX_FOCAL_CANDIDATES as i64 - 1) / 2);
    let candidates: Vec<(i64, f64)> = (-steps..=steps)
        .map(|i| (i, centre * FOCAL_STEP.powi(i as i32)))
        .collect();
    diagnostics.focal_candidates = candidates.len();
    let samples: Vec<Vec<((f64, f64), (f64, f64))>> = pairs
        .iter()
        .map(|p| match (&p.homography, p.verified) {
            (Some(fit), true) => spread(&fit.inliers, FOCAL_SAMPLES)
                .map(|i| p.points[i as usize])
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let scores: Vec<(usize, f64)> = candidates
        .par_iter()
        .map(|&(_, focal)| {
            cancel.check()?;
            let lens = Lens {
                focal_px: focal,
                radial,
                ..lens_bounds
            };
            let mut score = (0usize, 0.0f64);
            for points in samples.iter().filter(|s| !s.is_empty()) {
                let all: Vec<u32> = (0..points.len() as u32).collect();
                if let Some(relative) = fit_relative(&lens, points, &all) {
                    for &(p, q) in points {
                        let e = symmetric_error(&lens, &relative, p, q);
                        if e <= ROTATION_THRESHOLD_PX * px {
                            score.0 += 1;
                            score.1 += e * e;
                        }
                    }
                }
            }
            Ok(score)
        })
        .collect::<Result<_, Error>>()?;
    let best = (0..candidates.len())
        .min_by(|&i, &j| {
            scores[j]
                .0
                .cmp(&scores[i].0)
                .then(scores[i].1.total_cmp(&scores[j].1))
                .then(candidates[i].0.abs().cmp(&candidates[j].0.abs()))
                .then(i.cmp(&j))
        })
        .expect("at least one focal candidate");
    let mut lens = Lens {
        focal_px: candidates[best].1,
        radial,
        ..lens_bounds
    };

    // 4. Pairwise rotations at that focal length, and the spanning tree.
    cancel.check()?;
    let fitted: Vec<Option<(Rotation, Vec<u32>, usize)>> = pairs
        .par_iter()
        .map(|p| {
            cancel.check()?;
            Ok(match (&p.homography, p.verified) {
                (Some(fit), true) => classify(&lens, p, &fit.inliers, ROTATION_THRESHOLD_PX * px),
                _ => None,
            })
        })
        .collect::<Result<_, Error>>()?;
    for (pair, fit) in pairs.iter_mut().zip(fitted) {
        if let Some((relative, inliers, overlap)) = fit {
            if matching::verified(inliers.len(), overlap) {
                pair.overlap = overlap;
                pair.rotation = Some((relative, inliers));
            }
        }
    }
    let (mut rotations, mut solved, anchor) = spanning_tree(n, &pairs);

    // 5. Bundle adjustment rounds.
    let fit_radial = matches!(options.radial, RadialMode::Fit);
    let mut last_inliers: Vec<Option<Vec<u32>>> = pairs
        .iter()
        .map(|p| p.rotation.as_ref().map(|r| r.1.clone()))
        .collect();
    for _round in 0..MAX_BUNDLE_ROUNDS {
        cancel.check()?;
        progress(RegistrationStage::Bundle);
        cancel.check()?;
        let observations: Vec<PairObservations> = pairs
            .iter()
            .filter(|p| solved[p.a] && solved[p.b])
            .filter_map(|p| {
                let (_, inliers) = p.rotation.as_ref()?;
                let chosen: Vec<u32> = spread(inliers, MAX_BUNDLE_MATCHES).collect();
                Some(PairObservations {
                    a: p.a,
                    b: p.b,
                    points: chosen.iter().map(|&i| p.points[i as usize]).collect(),
                    weights: chosen
                        .iter()
                        .map(|&i| 0.25f64.powi(i32::from(p.levels[i as usize])))
                        .collect(),
                })
            })
            .collect();
        diagnostics.max_bundle_matches = diagnostics.max_bundle_matches.max(
            observations
                .iter()
                .map(|o| o.points.len())
                .max()
                .unwrap_or(0),
        );
        let adjusted = bundle::adjust(
            lens,
            rotations.clone(),
            &solved,
            anchor,
            &observations,
            fit_radial,
            HUBER_PX * px,
            MAX_BUNDLE_ITERATIONS,
            cancel,
        )?;
        diagnostics.bundle_rounds += 1;
        diagnostics.bundle_iterations.push(adjusted.iterations);
        lens = adjusted.lens;
        rotations = adjusted.rotations;

        // Classify every verified pair again under the adjusted model, placing frames not yet
        // placed from a verified pair to a placed frame.
        let placed_before = solved.clone();
        for index in 0..pairs.len() {
            cancel.check()?;
            let p = &pairs[index];
            if !p.verified {
                continue;
            }
            let (a, b) = (p.a, p.b);
            let relative = match (solved[a], solved[b]) {
                (true, true) => Some(rotations[b].relative_to(&rotations[a])),
                (true, false) | (false, true) => p
                    .homography
                    .as_ref()
                    .and_then(|fit| fit_relative(&lens, &p.points, &fit.inliers)),
                (false, false) => None,
            };
            let Some(relative) = relative else {
                pairs[index].rotation = None;
                continue;
            };
            let (inliers, overlap) =
                inliers_under(&lens, &relative, &p.points, INLIER_THRESHOLD_PX * px);
            let pair = &mut pairs[index];
            pair.overlap = overlap;
            if matching::verified(inliers.len(), overlap) {
                match (solved[a], solved[b]) {
                    (true, false) => {
                        rotations[b] = relative.then(&rotations[a]);
                        solved[b] = true;
                    }
                    (false, true) => {
                        rotations[a] = relative.inverse().then(&rotations[b]);
                        solved[a] = true;
                    }
                    _ => {}
                }
                pair.rotation = Some((relative, inliers));
            } else {
                pair.rotation = None;
            }
        }
        let inliers: Vec<Option<Vec<u32>>> = pairs
            .iter()
            .map(|p| p.rotation.as_ref().map(|r| r.1.clone()))
            .collect();
        let settled = inliers == last_inliers && solved == placed_before;
        last_inliers = inliers;
        if settled {
            break;
        }
    }

    // 6. Connectivity.
    let groups = components(
        n,
        pairs
            .iter()
            .filter(|p| p.rotation.is_some() && solved[p.a] && solved[p.b])
            .map(|p| (p.a, p.b)),
    );
    let main = largest(&groups);
    let left: Vec<usize> = (0..n).filter(|&i| groups[i] != main).collect();
    if !left.is_empty() {
        let linked = |i: usize| {
            pairs.iter().any(|p| {
                p.verified
                    && ((p.a == i && groups[p.b] == main) || (p.b == i && groups[p.a] == main))
            })
        };
        let (ambiguous, apart): (Vec<usize>, Vec<usize>) =
            left.into_iter().partition(|&i| linked(i));
        return Err(if apart.is_empty() {
            Refusal::AmbiguousRegistration { frames: ambiguous }
        } else {
            Refusal::Disconnected { frames: apart }
        }
        .into());
    }

    // Residuals under the final model.
    let mut squares = vec![(0.0f64, 0usize); n];
    let (mut total, mut count, mut max_residual) = (0.0f64, 0usize, 0.0f64);
    for p in pairs.iter_mut() {
        let Some((relative, inliers)) = &p.rotation else {
            continue;
        };
        let (mut sum, mut m) = (0.0, 0usize);
        for &i in inliers {
            let (from, to) = p.points[i as usize];
            for e in directional_errors(&lens, relative, from, to) {
                sum += e * e;
                m += 1;
                max_residual = max_residual.max(e);
            }
        }
        p.rms = if m > 0 { (sum / m as f64).sqrt() } else { 0.0 };
        for frame in [p.a, p.b] {
            squares[frame].0 += sum;
            squares[frame].1 += m;
        }
        total += sum;
        count += m;
    }

    // 7. Same view.
    for a in 0..n {
        for b in a + 1..n {
            if same_view(&lens, &rotations[a], &rotations[b]) {
                return Err(Refusal::SameView {
                    first: a,
                    second: b,
                }
                .into());
            }
        }
    }

    // 8. Levelling, then 9. the reference.
    cancel.check()?;
    progress(RegistrationStage::Levelling);
    cancel.check()?;
    let (rotations, levelling) = level(&rotations);
    let (rotations, reference, reference_origin) =
        centre_and_reference(&rotations, options.reference, &capture);

    let evidence: Vec<PairEvidence> = pairs
        .iter()
        .map(|p| {
            let (status, inliers) = match (&p.rotation, p.verified) {
                (Some((_, inliers)), _) => (PairStatus::Accepted, inliers.len()),
                (None, true) => {
                    // An inconsistent pair records its inliers under the final model too.
                    let relative = rotations[p.b].relative_to(&rotations[p.a]);
                    (
                        PairStatus::Inconsistent,
                        inliers_under(&lens, &relative, &p.points, INLIER_THRESHOLD_PX * px)
                            .0
                            .len(),
                    )
                }
                (None, false) => (PairStatus::Unmatched, 0),
            };
            PairEvidence {
                a: p.a,
                b: p.b,
                status,
                matches: p.points.len() as u32,
                overlap: p.overlap as u32,
                inliers: inliers as u32,
                inlier_ratio: if p.overlap > 0 {
                    inliers as f64 / p.overlap as f64
                } else {
                    0.0
                },
                rms_residual_px: p.rms,
            }
        })
        .collect();
    let frames_evidence = (0..n)
        .map(|i| FrameEvidence {
            features: frames[i].len() as u32,
            inliers: evidence
                .iter()
                .filter(|p| p.status == PairStatus::Accepted && (p.a == i || p.b == i))
                .map(|p| p.inliers)
                .sum(),
            rms_residual_px: if squares[i].1 > 0 {
                (squares[i].0 / squares[i].1 as f64).sqrt()
            } else {
                0.0
            },
        })
        .collect();
    Ok(PanoramaRegistration {
        lens,
        focal_origin,
        radial_origin,
        rotations,
        frames: frames_evidence,
        pairs: evidence,
        reference,
        reference_origin,
        levelling,
        rms_residual_px: if count > 0 {
            (total / count as f64).sqrt()
        } else {
            0.0
        },
        max_residual_px: max_residual,
        diagnostics,
    })
}

/// At most `limit` of `indices`, spread evenly over them in order.
fn spread(indices: &[u32], limit: usize) -> impl Iterator<Item = u32> + '_ {
    let n = indices.len();
    let take = n.min(limit);
    (0..take).map(move |k| indices[k * n / take.max(1)])
}

/// Connected components: each frame's component, numbered by its lowest frame.
fn components(n: usize, edges: impl Iterator<Item = (usize, usize)>) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut root = i;
        while parent[root] != root {
            root = parent[root];
        }
        let mut node = i;
        while parent[node] != root {
            let next = parent[node];
            parent[node] = root;
            node = next;
        }
        root
    }
    for (a, b) in edges {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }
    (0..n).map(|i| find(&mut parent, i)).collect()
}

/// The largest component, ties to the one holding the lowest frame.
fn largest(groups: &[usize]) -> usize {
    let mut sizes = vec![0usize; groups.len()];
    for &g in groups {
        sizes[g] += 1;
    }
    (0..groups.len())
        .max_by(|&i, &j| sizes[i].cmp(&sizes[j]).then(j.cmp(&i)))
        .expect("at least one frame")
}

/// Camera directions of a pair's points under `lens`, unit.
fn rays(lens: &Lens, p: (f64, f64)) -> V3 {
    linalg::normalize(lens.ray(p))
}

/// The rotation from the first frame's camera to the second's fitted to `inliers` of `points`.
fn fit_relative(
    lens: &Lens,
    points: &[((f64, f64), (f64, f64))],
    inliers: &[u32],
) -> Option<Rotation> {
    linalg::fit_rotation(inliers.iter().map(|&i| {
        let (p, q) = points[i as usize];
        (rays(lens, p), rays(lens, q), 1.0)
    }))
    .map(rotation_from)
}

/// A match's transfer errors under `relative` (first camera to second), each way, or infinity.
fn directional_errors(lens: &Lens, relative: &Rotation, p: (f64, f64), q: (f64, f64)) -> [f64; 2] {
    let forward = lens
        .project(relative.to_camera(lens.ray(p)))
        .map_or(f64::INFINITY, |m| (m.0 - q.0).hypot(m.1 - q.1));
    let backward = lens
        .project(relative.inverse().to_camera(lens.ray(q)))
        .map_or(f64::INFINITY, |m| (m.0 - p.0).hypot(m.1 - p.1));
    [forward, backward]
}

fn symmetric_error(lens: &Lens, relative: &Rotation, p: (f64, f64), q: (f64, f64)) -> f64 {
    let [f, b] = directional_errors(lens, relative, p, q);
    f.max(b)
}

/// The matches within `threshold` both ways under `relative`, and how many putative matches lie
/// in the overlap it implies.
fn inliers_under(
    lens: &Lens,
    relative: &Rotation,
    points: &[((f64, f64), (f64, f64))],
    threshold: f64,
) -> (Vec<u32>, usize) {
    let inverse = relative.inverse();
    let mut inliers = Vec::new();
    let mut overlap = 0;
    for (i, &(p, q)) in points.iter().enumerate() {
        let forward = lens.project(relative.to_camera(lens.ray(p)));
        let backward = lens.project(inverse.to_camera(lens.ray(q)));
        if let (Some(f), Some(b)) = (forward, backward) {
            if lens.contains(f) && lens.contains(b) {
                overlap += 1;
            }
            if (f.0 - q.0).hypot(f.1 - q.1) <= threshold
                && (b.0 - p.0).hypot(b.1 - p.1) <= threshold
            {
                inliers.push(i as u32);
            }
        }
    }
    (inliers, overlap)
}

/// A pair's rotation from its homography inliers, its inliers among all its matches within
/// `threshold`, refitted once: `(relative, inliers, overlap)`.
fn classify(
    lens: &Lens,
    pair: &PairState,
    start: &[u32],
    threshold: f64,
) -> Option<(Rotation, Vec<u32>, usize)> {
    let mut relative = fit_relative(lens, &pair.points, start)?;
    let (mut inliers, mut overlap) = inliers_under(lens, &relative, &pair.points, threshold);
    for _ in 0..2 {
        let Some(refit) = fit_relative(lens, &pair.points, &inliers) else {
            break;
        };
        let (next, next_overlap) = inliers_under(lens, &refit, &pair.points, threshold);
        if next.len() < inliers.len() {
            break;
        }
        relative = refit;
        inliers = next;
        overlap = next_overlap;
    }
    Some((relative, inliers, overlap))
}

/// Initial rotations from a maximum spanning tree of the rotation-accepted pairs by inlier count,
/// from the frame with the most inliers (ties to the lowest): `(rotations, placed, anchor)`.
fn spanning_tree(n: usize, pairs: &[PairState]) -> (Vec<Rotation>, Vec<bool>, usize) {
    let mut weight = vec![0usize; n];
    for p in pairs {
        if let Some((_, inliers)) = &p.rotation {
            weight[p.a] += inliers.len();
            weight[p.b] += inliers.len();
        }
    }
    let anchor = (0..n)
        .max_by(|&i, &j| weight[i].cmp(&weight[j]).then(j.cmp(&i)))
        .expect("at least one frame");
    let mut rotations = vec![Rotation::IDENTITY; n];
    let mut solved = vec![false; n];
    solved[anchor] = true;
    loop {
        let next = pairs
            .iter()
            .filter(|p| p.rotation.is_some() && solved[p.a] != solved[p.b])
            .max_by(|x, y| {
                let (wx, wy) = (
                    x.rotation.as_ref().unwrap().1.len(),
                    y.rotation.as_ref().unwrap().1.len(),
                );
                wx.cmp(&wy).then((y.a, y.b).cmp(&(x.a, x.b)))
            });
        let Some(p) = next else { break };
        let relative = p.rotation.as_ref().expect("filtered").0;
        if solved[p.a] {
            rotations[p.b] = relative.then(&rotations[p.a]);
            solved[p.b] = true;
        } else {
            rotations[p.a] = relative.inverse().then(&rotations[p.b]);
            solved[p.a] = true;
        }
    }
    (rotations, solved, anchor)
}

/// Whether two frames are the same view: every corner of each within [`SAME_VIEW_SHIFT`] of the
/// diagonal of the other's, and each covering [`SAME_VIEW_OVERLAP`] of the other.
fn same_view(lens: &Lens, a: &Rotation, b: &Rotation) -> bool {
    let limit = SAME_VIEW_SHIFT * lens.diagonal();
    let corners_close = lens.corners().iter().all(|&corner| {
        lens.transfer(a, b, corner)
            .is_some_and(|p| (p.0 - corner.0).hypot(p.1 - corner.1) <= limit)
    });
    if !corners_close {
        return false;
    }
    let covered = |to: &Rotation, from: &Rotation| {
        let (w, h) = (f64::from(lens.width) - 1.0, f64::from(lens.height) - 1.0);
        let mut inside = 0;
        for j in 0..OVERLAP_GRID {
            for i in 0..OVERLAP_GRID {
                let p = (
                    w * (i as f64 + 0.5) / OVERLAP_GRID as f64,
                    h * (j as f64 + 0.5) / OVERLAP_GRID as f64,
                );
                if lens.transfer(to, from, p).is_some_and(|q| lens.contains(q)) {
                    inside += 1;
                }
            }
        }
        inside as f64 / (OVERLAP_GRID * OVERLAP_GRID) as f64
    };
    covered(b, a) >= SAME_VIEW_OVERLAP && covered(a, b) >= SAME_VIEW_OVERLAP
}

/// `rotations` expressed in the world `m·w` (each `R·mᵀ`).
fn rebase(rotations: &[Rotation], m: &M3) -> Vec<Rotation> {
    let mt = linalg::transpose(m);
    rotations
        .iter()
        .map(|r| rotation_from(linalg::mul(&r.0, &mt)))
        .collect()
}

/// The rotation taking `down` to `+y`, keeping `forward`'s horizontal part as `+z`.
fn levelling_matrix(down: V3, forward: V3) -> M3 {
    let y = linalg::normalize(down);
    let mut z = linalg::sub(forward, linalg::scale(y, linalg::dot(forward, y)));
    if linalg::norm(z) < 1e-9 {
        // Looking straight along the vertical: any horizontal direction will do.
        let other = if y[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 0.0, 1.0]
        };
        z = linalg::cross(other, y);
    }
    let z = linalg::normalize(z);
    let x = linalg::cross(y, z);
    [x, y, z]
}

/// Level the rotations. See the module documentation.
fn level(rotations: &[Rotation]) -> (Vec<Rotation>, Levelling) {
    let n = rotations.len() as f64;
    let mut c = [[0.0f64; 3]; 3];
    let mut mean_down = [0.0f64; 3];
    let mut mean_forward = [0.0f64; 3];
    for r in rotations {
        let (x, y) = (r.right(), r.down());
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] +=
                    x[i] * x[j] + LEVELLING_WEIGHT * (f64::from(u8::from(i == j)) - y[i] * y[j]);
            }
        }
        mean_down = linalg::add(mean_down, y);
        mean_forward = linalg::add(mean_forward, r.forward());
    }
    let (_, vectors) = linalg::symmetric_eigen(c);
    let mut down = [vectors[0][0], vectors[1][0], vectors[2][0]];
    if linalg::dot(down, mean_down) < 0.0 {
        down = linalg::scale(down, -1.0);
    }
    let forward = if linalg::norm(mean_forward) > 1e-6 * n {
        mean_forward
    } else {
        rotations[0].forward()
    };
    let levelled = rebase(rotations, &levelling_matrix(down, forward));
    let rolls = |frames: &[Rotation]| -> Vec<(usize, f64, f64)> {
        frames
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let pitch = (-r.forward()[1]).clamp(-1.0, 1.0).asin().to_degrees();
                let roll = r.right()[1].clamp(-1.0, 1.0).asin().to_degrees();
                // A camera upside down has its right axis level but its down axis up.
                let roll = if r.down()[1] < 0.0 {
                    (180.0 - roll.abs()).copysign(roll)
                } else {
                    roll
                };
                (i, pitch, roll)
            })
            .collect()
    };
    let rolled = rolls(&levelled).into_iter().any(|(_, pitch, roll)| {
        pitch.abs() < LEVEL_CHECK_PITCH_DEG && roll.abs() > MAX_LEVEL_ROLL_DEG
    });
    let away = linalg::angle_between(down, mean_down).to_degrees() > MAX_LEVEL_ROLL_DEG;
    if !rolled && !away {
        return (levelled, Levelling::Levelled);
    }
    let fallback = rebase(rotations, &levelling_matrix(mean_down, forward));
    let (frame, _, roll_deg) = rolls(&fallback)
        .into_iter()
        .max_by(|a, b| a.2.abs().total_cmp(&b.2.abs()).then(b.0.cmp(&a.0)))
        .expect("at least one frame");
    (fallback, Levelling::Contradicted { frame, roll_deg })
}

/// A frame's yaw and pitch, in radians, in the levelled world.
fn yaw_pitch(r: &Rotation) -> (f64, f64) {
    let z = r.forward();
    (z[0].atan2(z[2]), (-z[1]).clamp(-1.0, 1.0).asin())
}

/// Turn the world so yaw 0 is the centre of the frames' yaw arc, and choose the reference.
fn centre_and_reference(
    rotations: &[Rotation],
    requested: Option<usize>,
    capture: &[usize],
) -> (Vec<Rotation>, usize, ReferenceOrigin) {
    let tau = std::f64::consts::TAU;
    let angles: Vec<(f64, f64)> = rotations.iter().map(yaw_pitch).collect();
    let mut yaws: Vec<f64> = angles
        .iter()
        .filter(|(_, pitch)| pitch.abs() < ARC_PITCH_LIMIT_DEG.to_radians())
        .map(|(yaw, _)| yaw.rem_euclid(tau))
        .collect();
    yaws.sort_by(f64::total_cmp);
    let centre_yaw = if yaws.is_empty() {
        0.0
    } else {
        // The arc starts after the largest gap between neighbouring yaws, ties to the first.
        let mut gap = (yaws[0] + tau - yaws[yaws.len() - 1], 0usize);
        for k in 1..yaws.len() {
            let g = yaws[k] - yaws[k - 1];
            if g > gap.0 {
                gap = (g, k);
            }
        }
        yaws[gap.1] + (tau - gap.0) / 2.0
    };
    let (s, c) = (-centre_yaw).sin_cos();
    let turn = [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]];
    let rotations = rebase(rotations, &turn);
    let pitches = angles.iter().map(|(_, pitch)| *pitch);
    let (lo, hi) = pitches.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
        (lo.min(p), hi.max(p))
    });
    let centre_pitch = 0.5 * (lo + hi);
    let centre = [0.0, -centre_pitch.sin(), centre_pitch.cos()];
    if let Some(reference) = requested {
        return (rotations, reference, ReferenceOrigin::Requested);
    }
    let distance: Vec<f64> = rotations
        .iter()
        .map(|r| linalg::angle_between(r.forward(), centre))
        .collect();
    let nearest = distance.iter().copied().fold(f64::INFINITY, f64::min);
    let reference = capture
        .iter()
        .copied()
        .find(|&i| distance[i] <= nearest + REFERENCE_TIE_DEG.to_radians())
        .expect("the nearest frame is within the tie");
    (rotations, reference, ReferenceOrigin::Centre)
}
