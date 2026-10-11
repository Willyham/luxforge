//! Exposure-robust global registration of each bracket input to the reference, from the inputs'
//! [`AnalysisProxy`]s alone.
//!
//! Coordinates are full-resolution upright pixel indices: pixel `(x, y)`'s centre is `(x, y)`. An
//! input's [`RigidTransform`] maps a reference point `p` to the input point that recorded it,
//! `q = c + R(θ)(p − c) + t`, with `c` the frame centre `((W − 1)/2, (H − 1)/2)`: a rotation and a
//! translation. Scale is not fitted: the bracket's focal length is equal by validation.
//!
//! The evidence is exposure invariant: each proxy pixel's `log2` luminance, where a stop of
//! exposure is a constant offset, compared only where both frames recorded it usably (above the
//! noise floor, below the highlight roll-off, [`super::proxy::PROXY_FLOOR`] and
//! [`super::proxy::PROXY_CEILING`]). Per input:
//!
//! 1. **Pyramid.** The proxy is halved by `[1 3 3 1]²/64` weighted means of its usable pixels
//!    (a 2 × 2 box under a binomial, against aliasing; a coarse pixel is usable when at least
//!    three of its 2 × 2 block are) until the longer side is at most [`COARSE_SIDE`], at most
//!    [`MAX_PYRAMID_LEVELS`] levels. The search reads the coarsest level smoothed once more by
//!    `[1 2 1]²/16`.
//! 2. **Search.** On the coarsest level, the zero-mean normalized correlation of the two images'
//!    ranks (each usable value's rank among its frame's usable values) over their shared usable
//!    pixels, at every whole-pixel translation within [`SEARCH_FRACTION`] of the level, then at
//!    every whole-degree rotation within [`SEARCH_ROTATION_DEG`] around the best translation.
//!    Ranks ignore the exposure offset and any monotone response, and bound what a moving subject
//!    far darker or brighter than the scene can pull the match by its share of the area. The best
//!    match must correlate by [`MIN_CORRELATION`]; another local peak [`DISTINCT_PEAK_PX`] away
//!    within [`DISTINCT_PEAK_MARGIN`] of it is an ambiguous match (a repeated pattern). The
//!    reference's log-luminance structure tensor, over its usable pixels and again over those the
//!    match shares, must have [`MIN_TEXTURE`] in its weaker direction.
//! 3. **Refinement.** From coarse to fine, Gauss-Newton on the rotation, translation and a
//!    log-brightness offset `β`, minimizing the Huber-weighted (`k = 1.345 σ`, `σ` the robust
//!    deviation, at least [`MIN_SIGMA_EV`]) residual `L_input(q) − L_reference(p) − β` over at most
//!    [`MAX_REGISTRATION_SAMPLES`] grid samples a level, bilinear in the input, for at most
//!    [`MAX_ITERATIONS`] iterations a level.
//! 4. **Acceptance.** On the proxy itself: at least [`MIN_SUPPORT`] shared usable samples,
//!    [`MIN_INLIER_FRACTION`] of them within [`INLIER_EV`] of the fit, a predicted one-sigma
//!    position error of at most [`MAX_UNCERTAINTY_PX`] proxy pixels, and a motion within
//!    [`MAX_ROTATION_DEG`] and [`MAX_SHIFT_FRACTION`] of each side. Otherwise a typed
//!    [`Refusal`].
//! 5. **Chaining.** An input the reference cannot fix (typically a wide bracket's extreme
//!    exposure, which shares only a narrow band of usable range with it) is fitted to an already
//!    aligned input instead, the one furthest from the reference in measured brightness first,
//!    and the two motions composed; the pair must pass the same acceptance and the composition
//!    the same motion limits. When no aligned input fits it, its refusal against the reference
//!    stands.
//!
//! With alignment off, every input keeps the identity, but the request still has to be framed as
//! one: one orientation, and no translation search on the coarsest level that finds the input
//! displaced by at least [`FRAMING_TOLERANCE_PX`] coarse pixels with a correlation clearly better
//! than in place ([`Refusal::FramingMismatch`]).
//!
//! Every level is bounded by the proxy (at most 1 Mi pixels); the scratch of one pair's two
//! pyramids and the input's gradients is at most about 24 bytes a proxy pixel (24 MiB), released
//! before the next input. `cancel` is checked before each input, level and iteration.

use super::exposure::median_of_sorted;
use super::frame::{FrameMetadata, MAX_INPUTS, MIN_INPUTS, Refusal};
use super::proxy::AnalysisProxy;
use crate::{Cancel, Error, ParameterDescriptor};
use serde::{Deserialize, Serialize};

/// The longest side of the coarsest pyramid level, where the search runs.
pub const COARSE_SIDE: usize = 64;
/// A pyramid level is not halved below this many pixels on its shorter side.
pub const MIN_LEVEL_SIDE: usize = 16;
/// The most pyramid levels, the proxy included.
pub const MAX_PYRAMID_LEVELS: usize = 6;
/// The search's translation range, as a fraction of the coarsest level's sides.
pub const SEARCH_FRACTION: f64 = 0.25;
/// The search's rotation range, in whole degrees either way.
pub const SEARCH_ROTATION_DEG: i32 = 4;
/// The search's best match must correlate at least this well.
pub const MIN_CORRELATION: f64 = 0.5;
/// Another local correlation peak at least this many coarse pixels from the best, and within
/// [`DISTINCT_PEAK_MARGIN`] of it, makes the match ambiguous.
pub const DISTINCT_PEAK_PX: i32 = 3;
pub const DISTINCT_PEAK_MARGIN: f64 = 0.02;
/// The weakest direction's mean squared log-luminance gradient on the coarsest level, in
/// (EV per coarse pixel)²: below it the scene cannot fix a position.
pub const MIN_TEXTURE: f64 = 1e-3;
/// The most samples one refinement level reads.
pub const MAX_REGISTRATION_SAMPLES: usize = 1 << 18;
/// The most Gauss-Newton iterations per level.
pub const MAX_ITERATIONS: usize = 30;
/// The smallest robust residual deviation the Huber weights assume, in EV.
pub const MIN_SIGMA_EV: f64 = 0.02;
/// The fewest shared usable samples on the proxy an accepted alignment rests on.
pub const MIN_SUPPORT: u64 = 1024;
/// A sample agrees with the fit when its residual is within this, in EV.
pub const INLIER_EV: f64 = 0.25;
/// The smallest fraction of shared samples that must agree with an accepted fit.
pub const MIN_INLIER_FRACTION: f64 = 0.5;
/// The largest predicted one-sigma position error of an accepted fit, in proxy pixels.
pub const MAX_UNCERTAINTY_PX: f64 = 0.1;
/// The largest rotation handheld alignment corrects.
pub const MAX_ROTATION_DEG: f64 = 3.0;
/// The largest translation it corrects, as a fraction of the frame's width or height.
pub const MAX_SHIFT_FRACTION: f64 = 0.08;
/// With alignment off, a displacement of at least this many coarse pixels...
pub const FRAMING_TOLERANCE_PX: i32 = 2;
/// ...that correlates better than in place by at least this refuses the request.
pub const FRAMING_MARGIN: f64 = 0.05;

/// Whether the merge registers its inputs to the reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AlignmentMode {
    /// Fit and correct each input's rotation and translation; refuse an unalignable bracket.
    #[default]
    Auto,
    /// Merge the inputs as framed, after checking they are framed as one.
    Off,
}

/// Whether the merge replaces moving regions with one exposure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeghostMode {
    /// Detect moving regions and show each from one exposure.
    #[default]
    Auto,
    /// Fuse every pixel from every exposure.
    Off,
}

impl AlignmentMode {
    pub const NAMES: [&'static str; 2] = ["auto", "off"];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

impl DeghostMode {
    pub const NAMES: [&'static str; 2] = ["auto", "off"];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

/// The merge's alignment and motion options, both automatic by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlignmentOptions {
    pub alignment: AlignmentMode,
    pub deghost: DeghostMode,
}

impl AlignmentOptions {
    /// The options as the command schema's parameter vocabulary describes them, one optional
    /// enumeration each with its default.
    pub fn parameters() -> Vec<ParameterDescriptor> {
        vec![
            ParameterDescriptor::enumeration("alignment", AlignmentMode::NAMES)
                .default(AlignmentMode::default().name())
                .notes(
                    "auto fits each exposure's rotation and translation to the reference and \
                     refuses an unalignable bracket; off merges the exposures as framed, after \
                     checking they share one framing",
                ),
            ParameterDescriptor::enumeration("deghost", DeghostMode::NAMES)
                .default(DeghostMode::default().name())
                .notes(
                    "auto shows each moving region from one exposure, the reference where it \
                     recorded the region usably; off fuses every exposure everywhere",
                ),
        ]
    }
}

/// A whole-pixel rectangle in full-resolution coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Whether `other` lies inside this rectangle.
    pub fn contains(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && u64::from(other.x) + u64::from(other.width)
                <= u64::from(self.x) + u64::from(self.width)
            && u64::from(other.y) + u64::from(other.height)
                <= u64::from(self.y) + u64::from(self.height)
    }
}

/// A rigid motion from reference coordinates to one input's, about the frame centre: see the
/// module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigidTransform {
    pub center_x: f64,
    pub center_y: f64,
    /// Radians; positive turns +x towards +y.
    pub rotation_rad: f64,
    pub shift_x: f64,
    pub shift_y: f64,
}

/// A transform's matrix form, `q = m · p + b`: the one evaluation resampling and coverage share.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Affine {
    pub m: [[f64; 2]; 2],
    pub b: [f64; 2],
}

impl Affine {
    #[inline]
    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.m[0][0] * x + self.m[0][1] * y + self.b[0],
            self.m[1][0] * x + self.m[1][1] * y + self.b[1],
        )
    }
}

impl RigidTransform {
    /// No motion, in a `width × height` frame.
    pub fn identity(width: u32, height: u32) -> Self {
        Self::new(width, height, 0.0, 0.0, 0.0)
    }

    pub fn new(width: u32, height: u32, rotation_rad: f64, shift_x: f64, shift_y: f64) -> Self {
        Self {
            center_x: (f64::from(width) - 1.0) / 2.0,
            center_y: (f64::from(height) - 1.0) / 2.0,
            rotation_rad,
            shift_x,
            shift_y,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.rotation_rad == 0.0 && self.shift_x == 0.0 && self.shift_y == 0.0
    }

    pub fn rotation_deg(&self) -> f64 {
        self.rotation_rad.to_degrees()
    }

    pub(crate) fn affine(&self) -> Affine {
        let (s, c) = self.rotation_rad.sin_cos();
        let m = [[c, -s], [s, c]];
        let (cx, cy) = (self.center_x, self.center_y);
        Affine {
            m,
            b: [
                cx + self.shift_x - (m[0][0] * cx + m[0][1] * cy),
                cy + self.shift_y - (m[1][0] * cx + m[1][1] * cy),
            ],
        }
    }

    /// The input point that recorded reference point `(x, y)`.
    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        self.affine().map(x, y)
    }

    /// This motion followed by `next`, about the same centre: `next(self(p))`.
    pub fn then(&self, next: &Self) -> Self {
        let (s, c) = next.rotation_rad.sin_cos();
        Self {
            center_x: self.center_x,
            center_y: self.center_y,
            rotation_rad: self.rotation_rad + next.rotation_rad,
            shift_x: c * self.shift_x - s * self.shift_y + next.shift_x,
            shift_y: s * self.shift_x + c * self.shift_y + next.shift_y,
        }
    }

    /// The largest distance any of `width × height`'s corners moves between this and `other`:
    /// one number for how far two estimates of a motion disagree.
    pub fn corner_distance(&self, other: &Self, width: u32, height: u32) -> f64 {
        let (w, h) = (f64::from(width) - 1.0, f64::from(height) - 1.0);
        [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
            .into_iter()
            .map(|(x, y)| {
                let (a, b) = (self.map(x, y), other.map(x, y));
                (a.0 - b.0).hypot(a.1 - b.1)
            })
            .fold(0.0, f64::max)
    }
}

/// What one input's alignment rests on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AlignmentEvidence {
    /// The reference, which defines the coordinates.
    Reference,
    /// A fitted motion: the aligned input it was chained through (`via`, when its exposure
    /// shares too little usable range with the reference's), the coarse search's correlation, the
    /// shared usable samples on the proxy, the fraction agreeing within [`INLIER_EV`], the robust
    /// residual deviation and measured log-brightness offset from the reference in EV, the
    /// predicted one-sigma position error in full-resolution pixels, and a confidence in
    /// `[0, 1]`, all of the last fitted pair.
    Fitted {
        via: Option<usize>,
        correlation: f64,
        samples: u64,
        inlier_fraction: f64,
        residual_ev: f64,
        brightness_ev: f64,
        uncertainty_px: f64,
        confidence: f64,
    },
    /// Alignment off: the best whole-pixel displacement the framing check found, in
    /// full-resolution pixels, and its correlation and the in-place correlation (absent where
    /// the frames share too little to correlate).
    Framing {
        shift_px: f64,
        correlation: Option<f64>,
        in_place: Option<f64>,
    },
}

/// One input's registration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAlignment {
    pub transform: RigidTransform,
    pub evidence: AlignmentEvidence,
}

/// The bracket's registration to its reference, in request order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub mode: AlignmentMode,
    pub reference: usize,
    pub width: u32,
    pub height: u32,
    pub inputs: Vec<InputAlignment>,
}

impl Registration {
    pub fn transforms(&self) -> Vec<RigidTransform> {
        self.inputs.iter().map(|input| input.transform).collect()
    }
}

/// Register every input's proxy to `reference`'s with `mode`. `frames` are the request's
/// validated metadata, in the same order as `proxies`. Unalignable or misframed inputs are
/// refused with a typed [`Refusal`], carried in the error's data.
pub fn register(
    frames: &[FrameMetadata],
    proxies: &[AnalysisProxy],
    reference: usize,
    mode: AlignmentMode,
    cancel: &Cancel,
) -> Result<Registration, Error> {
    let n = proxies.len();
    if !(MIN_INPUTS..=MAX_INPUTS).contains(&n) || frames.len() != n {
        return Err(Error::validation(
            "HDR alignment takes one proxy per validated input",
        ));
    }
    if reference >= n {
        return Err(Refusal::InvalidReference { reference }.into());
    }
    let (width, height) = proxies[reference].frame_dimensions();
    for (frame, proxy) in frames.iter().zip(proxies) {
        if proxy.frame_dimensions() != (width, height)
            || (frame.width, frame.height) != (width, height)
        {
            return Err(Error::validation(
                "HDR alignment proxies differ from the bracket's dimensions",
            ));
        }
    }
    let identity = RigidTransform::identity(width, height);
    let reference_pyramid = Pyramid::new(&proxies[reference], false, cancel)?;
    let mut inputs: Vec<Option<InputAlignment>> = vec![None; n];
    inputs[reference] = Some(InputAlignment {
        transform: identity,
        evidence: AlignmentEvidence::Reference,
    });
    // Inputs the reference could not fix, with its refusal, for the chained pass.
    let mut unsupported = Vec::new();
    for input in 0..n {
        cancel.check()?;
        if input == reference {
            continue;
        }
        let pyramid = Pyramid::new(&proxies[input], mode == AlignmentMode::Auto, cancel)?;
        inputs[input] = Some(match mode {
            AlignmentMode::Auto => {
                match fit(
                    input,
                    None,
                    &reference_pyramid,
                    &pyramid,
                    width,
                    height,
                    cancel,
                ) {
                    Ok(alignment) => alignment,
                    Err(Failure::Refused(refusal)) => {
                        unsupported.push((input, refusal));
                        continue;
                    }
                    Err(failure) => return Err(failure.into()),
                }
            }
            AlignmentMode::Off => {
                if frames[input].orientation != frames[reference].orientation {
                    return Err(Refusal::FramingMismatch {
                        input,
                        shift_px: None,
                    }
                    .into());
                }
                InputAlignment {
                    transform: identity,
                    evidence: framing(input, &reference_pyramid, &pyramid)?,
                }
            }
        });
    }
    drop(reference_pyramid);
    // Chain each remaining input through an aligned one, the furthest from the reference in
    // brightness first (the likeliest to share its range), until none is left or none fits.
    while !unsupported.is_empty() {
        let mut progressed = false;
        let mut remaining = Vec::new();
        for (input, refusal) in unsupported {
            let mut vias: Vec<(usize, f64)> = inputs
                .iter()
                .enumerate()
                .filter_map(|(via, alignment)| match alignment.as_ref()?.evidence {
                    AlignmentEvidence::Fitted { brightness_ev, .. } => Some((via, brightness_ev)),
                    _ => None,
                })
                .collect();
            vias.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then(a.0.cmp(&b.0)));
            let pyramid = Pyramid::new(&proxies[input], true, cancel)?;
            let mut chained = None;
            for (via, via_brightness) in vias {
                cancel.check()?;
                let via_pyramid = Pyramid::new(&proxies[via], false, cancel)?;
                match fit(
                    input,
                    Some(via),
                    &via_pyramid,
                    &pyramid,
                    width,
                    height,
                    cancel,
                ) {
                    Ok(mut alignment) => {
                        let first = inputs[via].as_ref().expect("an aligned via").transform;
                        alignment.transform = first.then(&alignment.transform);
                        if let AlignmentEvidence::Fitted { brightness_ev, .. } =
                            &mut alignment.evidence
                        {
                            *brightness_ev += via_brightness;
                        }
                        // A chain that composes past the limits is not this input's motion.
                        if severe(input, &alignment.transform, width, height).is_ok() {
                            chained = Some(alignment);
                            break;
                        }
                    }
                    Err(Failure::Refused(_)) => {}
                    Err(Failure::Error(error)) => return Err(error),
                }
            }
            match chained {
                Some(alignment) => {
                    inputs[input] = Some(alignment);
                    progressed = true;
                }
                None => remaining.push((input, refusal)),
            }
        }
        if !progressed {
            let (_, refusal) = remaining.swap_remove(0);
            return Err(refusal.into());
        }
        unsupported = remaining;
    }
    let inputs = inputs
        .into_iter()
        .map(|alignment| alignment.expect("every input aligned"))
        .collect();
    Ok(Registration {
        mode,
        reference,
        width,
        height,
        inputs,
    })
}

/// One pyramid level: log luminance and usability, with the full-resolution position of its
/// pixels, `P = scale · p + offset`.
#[derive(Debug)]
struct Level {
    width: usize,
    height: usize,
    scale: f64,
    offset: f64,
    value: Vec<f32>,
    usable: Vec<bool>,
    /// Central-difference gradients where both neighbours are usable, else NaN; only for the
    /// moving input.
    gradient: Option<[Vec<f32>; 2]>,
}

impl Level {
    #[inline]
    fn at(&self, x: usize, y: usize) -> Option<f32> {
        let i = y * self.width + x;
        self.usable[i].then_some(self.value[i])
    }

    /// The bilinear sample at `(x, y)` when its four pixels are usable, with its weights' base.
    #[inline]
    fn taps(&self, x: f64, y: f64) -> Option<(usize, f64, f64)> {
        if !(x >= 0.0 && y >= 0.0 && x <= (self.width - 1) as f64 && y <= (self.height - 1) as f64)
        {
            return None;
        }
        let x0 = (x.floor() as usize).min(self.width - 2);
        let y0 = (y.floor() as usize).min(self.height - 2);
        let i = y0 * self.width + x0;
        let w = self.width;
        (self.usable[i] && self.usable[i + 1] && self.usable[i + w] && self.usable[i + w + 1])
            .then_some((i, x - x0 as f64, y - y0 as f64))
    }

    #[inline]
    fn blend(plane: &[f32], i: usize, w: usize, fx: f64, fy: f64) -> f64 {
        let a = f64::from(plane[i]);
        let b = f64::from(plane[i + 1]);
        let c = f64::from(plane[i + w]);
        let d = f64::from(plane[i + w + 1]);
        (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy
    }

    #[inline]
    fn sample(&self, x: f64, y: f64) -> Option<f64> {
        let (i, fx, fy) = self.taps(x, y)?;
        Some(Self::blend(&self.value, i, self.width, fx, fy))
    }

    /// The sample and its gradient at `(x, y)`.
    #[inline]
    fn sample_gradient(&self, x: f64, y: f64) -> Option<(f64, f64, f64)> {
        let (i, fx, fy) = self.taps(x, y)?;
        let [gx, gy] = self.gradient.as_ref()?;
        let gx = Self::blend(gx, i, self.width, fx, fy);
        let gy = Self::blend(gy, i, self.width, fx, fy);
        (gx.is_finite() && gy.is_finite())
            .then(|| (Self::blend(&self.value, i, self.width, fx, fy), gx, gy))
    }

    /// The next level: each pixel the `[1 3 3 1]² / 64` weighted mean (a 2 × 2 box under a
    /// binomial, against aliasing) of the usable pixels around its 2 × 2 block, usable when at
    /// least three of the block's four are.
    fn halve(&self) -> Self {
        let (width, height) = (self.width / 2, self.height / 2);
        let mut value = vec![0.0; width * height];
        let mut usable = vec![false; width * height];
        const TAPS: [f64; 4] = [1.0, 3.0, 3.0, 1.0];
        for y in 0..height {
            for x in 0..width {
                let block = [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .into_iter()
                    .filter(|(dx, dy)| self.at(2 * x + dx, 2 * y + dy).is_some())
                    .count();
                if block < 3 {
                    continue;
                }
                let (mut sum, mut weight) = (0.0_f64, 0.0_f64);
                for (ky, wy) in TAPS.iter().enumerate() {
                    let Some(sy) = (2 * y + ky).checked_sub(1).filter(|&sy| sy < self.height)
                    else {
                        continue;
                    };
                    for (kx, wx) in TAPS.iter().enumerate() {
                        let Some(sx) = (2 * x + kx).checked_sub(1).filter(|&sx| sx < self.width)
                        else {
                            continue;
                        };
                        if let Some(v) = self.at(sx, sy) {
                            sum += wx * wy * f64::from(v);
                            weight += wx * wy;
                        }
                    }
                }
                value[y * width + x] = (sum / weight) as f32;
                usable[y * width + x] = true;
            }
        }
        let scale = 2.0 * self.scale;
        Self {
            width,
            height,
            scale,
            offset: (scale - 1.0) / 2.0,
            value,
            usable,
            gradient: None,
        }
    }

    /// The level smoothed by the binomial `[1 2 1]²/16` over its usable pixels, usable where it
    /// was: the search's view, so a whole-pixel shift is not defeated by detail near the level's
    /// sampling limit.
    fn smoothed(&self) -> Self {
        let (w, h) = (self.width, self.height);
        let mut value = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if !self.usable[i] {
                    continue;
                }
                let (mut sum, mut weight) = (0.0_f64, 0.0_f64);
                for ny in y.saturating_sub(1)..(y + 2).min(h) {
                    for nx in x.saturating_sub(1)..(x + 2).min(w) {
                        if let Some(v) = self.at(nx, ny) {
                            let k = ((2 - nx.abs_diff(x)) * (2 - ny.abs_diff(y))) as f64;
                            sum += k * f64::from(v);
                            weight += k;
                        }
                    }
                }
                value[i] = (sum / weight) as f32;
            }
        }
        Self {
            width: w,
            height: h,
            scale: self.scale,
            offset: self.offset,
            value,
            usable: self.usable.clone(),
            gradient: None,
        }
    }

    /// The level with each usable value replaced by its rank among the usable values, over
    /// `0..=1` (ties by position): invariant to exposure and to any monotone response, and no
    /// pixel, however far from the rest, moves a correlation by more than its share of the area.
    fn ranked(&self) -> Self {
        let mut order: Vec<usize> = (0..self.value.len()).filter(|&i| self.usable[i]).collect();
        order.sort_by(|&a, &b| self.value[a].total_cmp(&self.value[b]).then(a.cmp(&b)));
        let last = order.len().saturating_sub(1).max(1) as f64;
        let mut value = vec![0.0; self.value.len()];
        for (rank, &i) in order.iter().enumerate() {
            value[i] = (rank as f64 / last) as f32;
        }
        Self {
            width: self.width,
            height: self.height,
            scale: self.scale,
            offset: self.offset,
            value,
            usable: self.usable.clone(),
            gradient: None,
        }
    }

    fn with_gradient(mut self) -> Self {
        let (w, h) = (self.width, self.height);
        let mut gx = vec![f32::NAN; w * h];
        let mut gy = vec![f32::NAN; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if x > 0 && x + 1 < w && self.usable[i - 1] && self.usable[i + 1] {
                    gx[i] = (self.value[i + 1] - self.value[i - 1]) / 2.0;
                }
                if y > 0 && y + 1 < h && self.usable[i - w] && self.usable[i + w] {
                    gy[i] = (self.value[i + w] - self.value[i - w]) / 2.0;
                }
            }
        }
        self.gradient = Some([gx, gy]);
        self
    }

    /// The point of this level an input records reference level point `(x, y)` at, under
    /// `affine`, with the full-resolution point it stands for.
    #[inline]
    fn map(&self, affine: &Affine, x: f64, y: f64) -> ((f64, f64), (f64, f64)) {
        let p = (self.scale * x + self.offset, self.scale * y + self.offset);
        let q = affine.map(p.0, p.1);
        (
            (
                (q.0 - self.offset) / self.scale,
                (q.1 - self.offset) / self.scale,
            ),
            p,
        )
    }
}

/// A proxy's levels, finest first, the coarsest level smoothed, and its ranks for the search.
#[derive(Debug)]
struct Pyramid {
    levels: Vec<Level>,
    coarse: Level,
    ranks: Level,
}

impl Pyramid {
    fn new(proxy: &AnalysisProxy, gradients: bool, cancel: &Cancel) -> Result<Self, Error> {
        let scale = f64::from(proxy.factor);
        let base = Level {
            width: proxy.width,
            height: proxy.height,
            scale,
            offset: (scale - 1.0) / 2.0,
            value: proxy.log_luma.clone(),
            usable: (0..proxy.width * proxy.height)
                .map(|i| proxy.usable(i))
                .collect(),
            gradient: None,
        };
        let mut levels = vec![base];
        loop {
            let last = levels.last().expect("a base level");
            if levels.len() >= MAX_PYRAMID_LEVELS
                || last.width.max(last.height) <= COARSE_SIDE
                || last.width.min(last.height) / 2 < MIN_LEVEL_SIDE
            {
                break;
            }
            cancel.check()?;
            let next = last.halve();
            levels.push(next);
        }
        let coarse = levels.last().expect("a base level").smoothed();
        let ranks = coarse.ranked();
        if gradients {
            levels = levels.into_iter().map(Level::with_gradient).collect();
        }
        Ok(Self {
            levels,
            coarse,
            ranks,
        })
    }
}

/// The zero-mean normalized correlation of the reference and the input moved by a whole
/// `(dx, dy)` coarse pixels, over the pixels both recorded usably; `None` below `min_overlap`.
fn correlate_shift(
    reference: &Level,
    input: &Level,
    dx: i32,
    dy: i32,
    min_overlap: usize,
) -> Option<f64> {
    let mut stats = Moments::default();
    let (w, h) = (reference.width as i32, reference.height as i32);
    for y in 0.max(-dy)..h.min(h - dy) {
        for x in 0.max(-dx)..w.min(w - dx) {
            let (Some(a), Some(b)) = (
                reference.at(x as usize, y as usize),
                input.at((x + dx) as usize, (y + dy) as usize),
            ) else {
                continue;
            };
            stats.add(f64::from(a), f64::from(b));
        }
    }
    stats.correlation(min_overlap)
}

/// The correlation of the reference with the input under `affine`, bilinear in the input.
fn correlate_affine(
    reference: &Level,
    input: &Level,
    affine: &Affine,
    min_overlap: usize,
) -> Option<f64> {
    let mut stats = Moments::default();
    for y in 0..reference.height {
        for x in 0..reference.width {
            let Some(a) = reference.at(x, y) else {
                continue;
            };
            let (q, _) = reference.map(affine, x as f64, y as f64);
            let Some(b) = input.sample(q.0, q.1) else {
                continue;
            };
            stats.add(f64::from(a), b);
        }
    }
    stats.correlation(min_overlap)
}

#[derive(Default)]
struct Moments {
    n: usize,
    a: f64,
    b: f64,
    aa: f64,
    bb: f64,
    ab: f64,
}

impl Moments {
    #[inline]
    fn add(&mut self, a: f64, b: f64) {
        self.n += 1;
        self.a += a;
        self.b += b;
        self.aa += a * a;
        self.bb += b * b;
        self.ab += a * b;
    }

    fn correlation(&self, min_overlap: usize) -> Option<f64> {
        if self.n < min_overlap.max(2) {
            return None;
        }
        let n = self.n as f64;
        let va = self.aa - self.a * self.a / n;
        let vb = self.bb - self.b * self.b / n;
        let cov = self.ab - self.a * self.b / n;
        if va <= 1e-12 * n || vb <= 1e-12 * n {
            return Some(0.0);
        }
        Some(cov / (va * vb).sqrt())
    }
}

/// The translation search's correlation surface on the coarsest level.
struct Surface {
    radius: (i32, i32),
    scores: Vec<Option<f64>>,
}

impl Surface {
    fn new(reference: &Level, input: &Level) -> Self {
        let radius = (
            (reference.width as f64 * SEARCH_FRACTION).ceil() as i32,
            (reference.height as f64 * SEARCH_FRACTION).ceil() as i32,
        );
        let min_overlap = min_overlap(reference);
        let mut scores = Vec::with_capacity(((2 * radius.0 + 1) * (2 * radius.1 + 1)) as usize);
        for dy in -radius.1..=radius.1 {
            for dx in -radius.0..=radius.0 {
                scores.push(correlate_shift(reference, input, dx, dy, min_overlap));
            }
        }
        Self { radius, scores }
    }

    fn at(&self, dx: i32, dy: i32) -> Option<f64> {
        if dx.abs() > self.radius.0 || dy.abs() > self.radius.1 {
            return None;
        }
        let row = (2 * self.radius.0 + 1) as usize;
        self.scores[(dy + self.radius.1) as usize * row + (dx + self.radius.0) as usize]
    }

    /// The best shift, the first in row order among equals.
    fn best(&self) -> Option<((i32, i32), f64)> {
        let mut best: Option<((i32, i32), f64)> = None;
        for dy in -self.radius.1..=self.radius.1 {
            for dx in -self.radius.0..=self.radius.0 {
                if let Some(score) = self.at(dx, dy)
                    && best.is_none_or(|(_, b)| score > b)
                {
                    best = Some(((dx, dy), score));
                }
            }
        }
        best
    }

    /// The best local maximum at least [`DISTINCT_PEAK_PX`] from `peak`.
    fn rival(&self, peak: (i32, i32)) -> Option<f64> {
        let mut rival: Option<f64> = None;
        for dy in -self.radius.1..=self.radius.1 {
            for dx in -self.radius.0..=self.radius.0 {
                if (dx - peak.0).abs().max((dy - peak.1).abs()) < DISTINCT_PEAK_PX {
                    continue;
                }
                let Some(score) = self.at(dx, dy) else {
                    continue;
                };
                let local = (-1..=1).all(|ny| {
                    (-1..=1).all(|nx| {
                        (nx == 0 && ny == 0)
                            || self.at(dx + nx, dy + ny).is_none_or(|other| other < score)
                    })
                });
                if local && rival.is_none_or(|r| score > r) {
                    rival = Some(score);
                }
            }
        }
        rival
    }
}

/// The fewest shared pixels one coarse correlation counts: a quarter of the level, at least 64.
fn min_overlap(level: &Level) -> usize {
    (level.width * level.height / 4).max(64)
}

/// Alignment off: the input must not be visibly displaced.
fn framing(
    input: usize,
    reference: &Pyramid,
    pyramid: &Pyramid,
) -> Result<AlignmentEvidence, Refusal> {
    let surface = Surface::new(&reference.ranks, &pyramid.ranks);
    let coarse_reference = &reference.coarse;
    let in_place = surface.at(0, 0);
    let best = surface.best();
    let scale = coarse_reference.scale;
    let shift_px = best
        .map(|((dx, dy), _)| scale * f64::from(dx).hypot(f64::from(dy)))
        .unwrap_or(0.0);
    if let Some(((dx, dy), score)) = best
        && dx.abs().max(dy.abs()) >= FRAMING_TOLERANCE_PX
        && score >= MIN_CORRELATION
        && in_place.is_none_or(|in_place| score >= in_place + FRAMING_MARGIN)
    {
        return Err(Refusal::FramingMismatch {
            input,
            shift_px: Some(shift_px),
        });
    }
    Ok(AlignmentEvidence::Framing {
        shift_px,
        correlation: best.map(|(_, score)| score),
        in_place,
    })
}

/// The refinement's parameters: rotation, translation (full-resolution pixels) and brightness.
#[derive(Clone, Copy, Debug)]
struct Parameters {
    rotation: f64,
    shift: (f64, f64),
    brightness: f64,
}

impl Parameters {
    fn transform(&self, width: u32, height: u32) -> RigidTransform {
        RigidTransform::new(width, height, self.rotation, self.shift.0, self.shift.1)
    }
}

/// One refinement pass's accumulated normal equations and residual statistics.
struct Normal {
    h: [[f64; 4]; 4],
    g: [f64; 4],
    residuals: Vec<f64>,
}

/// Why one pair did not fit: a refusal, which another pair may overcome, or an error.
enum Failure {
    Refused(Refusal),
    Error(Error),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Error(error)
    }
}

impl From<Failure> for Error {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Refused(refusal) => refusal.into(),
            Failure::Error(error) => error,
        }
    }
}

/// Fit `input` to the frame of `reference`'s pyramid, which is the bracket's reference or, for a
/// chained input, the aligned input it goes `via`.
fn fit(
    input: usize,
    via: Option<usize>,
    reference: &Pyramid,
    pyramid: &Pyramid,
    width: u32,
    height: u32,
    cancel: &Cancel,
) -> Result<InputAlignment, Failure> {
    let (coarse_reference, coarse) = (&reference.coarse, &pyramid.coarse);
    texture(input, coarse_reference, None)?;
    // Search on ranks: translation, then rotation around the best translation.
    let surface = Surface::new(&reference.ranks, &pyramid.ranks);
    let Some(((dx, dy), translation_score)) = surface.best() else {
        return Err(Refusal::AlignmentSupport { input, samples: 0 }.into());
    };
    if translation_score < MIN_CORRELATION {
        return Err(Refusal::AmbiguousAlignment {
            input,
            correlation: translation_score,
            inlier_fraction: None,
        }
        .into());
    }
    if let Some(rival) = surface.rival((dx, dy))
        && rival >= translation_score - DISTINCT_PEAK_MARGIN
    {
        return Err(Refusal::AmbiguousAlignment {
            input,
            correlation: translation_score,
            inlier_fraction: None,
        }
        .into());
    }
    cancel.check()?;
    let scale = coarse_reference.scale;
    let overlap = min_overlap(coarse_reference);
    let mut start = Parameters {
        rotation: 0.0,
        shift: (scale * f64::from(dx), scale * f64::from(dy)),
        brightness: 0.0,
    };
    let mut correlation = translation_score;
    for degrees in -SEARCH_ROTATION_DEG..=SEARCH_ROTATION_DEG {
        if degrees == 0 {
            continue;
        }
        let rotation = f64::from(degrees).to_radians();
        for ny in -1..=1 {
            for nx in -1..=1 {
                let candidate = Parameters {
                    rotation,
                    shift: (scale * f64::from(dx + nx), scale * f64::from(dy + ny)),
                    brightness: 0.0,
                };
                let affine = candidate.transform(width, height).affine();
                if let Some(score) =
                    correlate_affine(&reference.ranks, &pyramid.ranks, &affine, overlap)
                    && score > correlation
                {
                    correlation = score;
                    start = candidate;
                }
            }
        }
    }
    let affine = start.transform(width, height).affine();
    texture(input, coarse_reference, Some((coarse, &affine)))?;
    // Refinement, coarse to fine.
    let mut parameters = start;
    parameters.brightness = brightness_offset(coarse_reference, coarse, &start, width, height)
        .ok_or(Refusal::AlignmentSupport { input, samples: 0 })?;
    let limit = (
        2.0 * MAX_SHIFT_FRACTION * f64::from(width),
        2.0 * MAX_SHIFT_FRACTION * f64::from(height),
    );
    for (reference_level, level) in reference.levels.iter().zip(&pyramid.levels).rev() {
        cancel.check()?;
        let mut sigma = MIN_SIGMA_EV.max(robust_sigma(
            &mut normal(
                reference_level,
                level,
                &parameters,
                width,
                height,
                f64::INFINITY,
            )
            .residuals,
        ));
        for _ in 0..MAX_ITERATIONS {
            cancel.check()?;
            let normal = normal(reference_level, level, &parameters, width, height, sigma);
            let Some(step) = solve4(normal.h, normal.g) else {
                return Err(Refusal::LowTexture { input }.into());
            };
            // At most two level pixels a step, so a poor start cannot leap away.
            let cap = 2.0 * level.scale;
            let shift = (step[1].clamp(-cap, cap), step[2].clamp(-cap, cap));
            parameters.rotation -= step[0].clamp(-0.02, 0.02);
            parameters.shift.0 -= shift.0;
            parameters.shift.1 -= shift.1;
            parameters.brightness -= step[3];
            if parameters.shift.0.abs() > limit.0
                || parameters.shift.1.abs() > limit.1
                || parameters.rotation.abs() > 2.0 * MAX_ROTATION_DEG.to_radians()
            {
                break;
            }
            sigma = MIN_SIGMA_EV.max(robust_sigma(&mut { normal.residuals }));
            let radius = level.width.max(level.height) as f64 * level.scale / 2.0;
            if shift.0.abs().max(shift.1.abs()) < 1e-3 * level.scale
                && step[0].abs() * radius < 1e-3 * level.scale
            {
                break;
            }
        }
    }
    let transform = parameters.transform(width, height);
    severe(input, &transform, width, height)?;
    // Acceptance on the proxy itself.
    let (base_reference, base) = (&reference.levels[0], &pyramid.levels[0]);
    let mut final_normal = normal(
        base_reference,
        base,
        &parameters,
        width,
        height,
        f64::INFINITY,
    );
    let samples = final_normal.residuals.len() as u64;
    if samples < MIN_SUPPORT {
        return Err(Refusal::AlignmentSupport { input, samples }.into());
    }
    let inliers = final_normal
        .residuals
        .iter()
        .filter(|r| r.abs() <= INLIER_EV)
        .count();
    let inlier_fraction = inliers as f64 / samples as f64;
    if inlier_fraction < MIN_INLIER_FRACTION {
        return Err(Refusal::AmbiguousAlignment {
            input,
            correlation,
            inlier_fraction: Some(inlier_fraction),
        }
        .into());
    }
    let residual_ev = robust_sigma(&mut final_normal.residuals);
    let weighted = normal(
        base_reference,
        base,
        &parameters,
        width,
        height,
        MIN_SIGMA_EV.max(residual_ev),
    );
    let weakest = weakest_direction(&weighted.h, base);
    let uncertainty = if weakest > 0.0 {
        MIN_SIGMA_EV.max(residual_ev) / weakest.sqrt()
    } else {
        f64::INFINITY
    };
    if uncertainty > MAX_UNCERTAINTY_PX {
        return Err(Refusal::LowTexture { input }.into());
    }
    let confidence = (1.0 - uncertainty / MAX_UNCERTAINTY_PX)
        .min((inlier_fraction - MIN_INLIER_FRACTION) / (1.0 - MIN_INLIER_FRACTION))
        .clamp(0.0, 1.0);
    Ok(InputAlignment {
        transform,
        evidence: AlignmentEvidence::Fitted {
            via,
            correlation,
            samples,
            inlier_fraction,
            residual_ev,
            brightness_ev: parameters.brightness,
            uncertainty_px: uncertainty * base.scale,
            confidence,
        },
    })
}

/// Refuse a motion beyond [`MAX_ROTATION_DEG`] or [`MAX_SHIFT_FRACTION`].
fn severe(
    input: usize,
    transform: &RigidTransform,
    width: u32,
    height: u32,
) -> Result<(), Refusal> {
    if transform.rotation_deg().abs() > MAX_ROTATION_DEG
        || transform.shift_x.abs() > MAX_SHIFT_FRACTION * f64::from(width)
        || transform.shift_y.abs() > MAX_SHIFT_FRACTION * f64::from(height)
    {
        return Err(Refusal::SevereMisalignment {
            input,
            shift_px: transform.shift_x.hypot(transform.shift_y),
            rotation_deg: transform.rotation_deg(),
        });
    }
    Ok(())
}

/// Refuse a scene whose reference has too little structure in some direction, over its usable
/// pixels or only those `shared` with the input under a motion, on the coarsest level where noise
/// has averaged away.
fn texture(
    input: usize,
    reference: &Level,
    shared: Option<(&Level, &Affine)>,
) -> Result<(), Refusal> {
    let (mut xx, mut xy, mut yy, mut n) = (0.0_f64, 0.0_f64, 0.0_f64, 0_usize);
    let w = reference.width;
    for y in 1..reference.height - 1 {
        for x in 1..w - 1 {
            let (Some(left), Some(right), Some(up), Some(down)) = (
                reference.at(x - 1, y),
                reference.at(x + 1, y),
                reference.at(x, y - 1),
                reference.at(x, y + 1),
            ) else {
                continue;
            };
            if let Some((level, affine)) = shared {
                let (q, _) = reference.map(affine, x as f64, y as f64);
                if level.sample(q.0, q.1).is_none() {
                    continue;
                }
            }
            let gx = f64::from(right - left) / 2.0;
            let gy = f64::from(down - up) / 2.0;
            xx += gx * gx;
            xy += gx * gy;
            yy += gy * gy;
            n += 1;
        }
    }
    if n < 16 {
        return Err(Refusal::AlignmentSupport {
            input,
            samples: n as u64,
        });
    }
    let (xx, xy, yy) = (xx / n as f64, xy / n as f64, yy / n as f64);
    let weakest = (xx + yy) / 2.0 - (((xx - yy) / 2.0).powi(2) + xy * xy).sqrt();
    if weakest < MIN_TEXTURE {
        return Err(Refusal::LowTexture { input });
    }
    Ok(())
}

/// The median of `L_input(q) − L_reference(p)` over the shared pixels of one level.
fn brightness_offset(
    reference: &Level,
    level: &Level,
    parameters: &Parameters,
    width: u32,
    height: u32,
) -> Option<f64> {
    let affine = parameters.transform(width, height).affine();
    let mut differences = Vec::new();
    for y in 0..reference.height {
        for x in 0..reference.width {
            let Some(a) = reference.at(x, y) else {
                continue;
            };
            let (q, _) = reference.map(&affine, x as f64, y as f64);
            if let Some(b) = level.sample(q.0, q.1) {
                differences.push(b - f64::from(a));
            }
        }
    }
    middle(&mut differences)
}

/// The median of `values`, sorting them in place; `None` when empty.
fn middle(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f64::total_cmp);
    Some(median_of_sorted(values))
}

/// `1.4826 · median |r − median r|`.
fn robust_sigma(residuals: &mut [f64]) -> f64 {
    let Some(centre) = middle(residuals) else {
        return 0.0;
    };
    for r in residuals.iter_mut() {
        *r = (*r - centre).abs();
    }
    1.4826 * middle(residuals).unwrap_or(0.0)
}

/// The Huber-weighted normal equations of one level at `parameters`, over a grid of at most
/// [`MAX_REGISTRATION_SAMPLES`] reference pixels, and every counted sample's residual. The
/// unknowns are the rotation, the translation and the brightness; the equations are for the
/// step to subtract.
fn normal(
    reference: &Level,
    level: &Level,
    parameters: &Parameters,
    width: u32,
    height: u32,
    sigma: f64,
) -> Normal {
    let transform = parameters.transform(width, height);
    let affine = transform.affine();
    let (s, c) = parameters.rotation.sin_cos();
    let stride = ((reference.width * reference.height) as f64 / MAX_REGISTRATION_SAMPLES as f64)
        .sqrt()
        .ceil()
        .max(1.0) as usize;
    let k = 1.345 * sigma;
    let mut h = [[0.0_f64; 4]; 4];
    let mut g = [0.0_f64; 4];
    let mut residuals = Vec::new();
    for y in (0..reference.height).step_by(stride) {
        for x in (0..reference.width).step_by(stride) {
            let Some(a) = reference.at(x, y) else {
                continue;
            };
            let (q, p) = reference.map(&affine, x as f64, y as f64);
            let Some((b, gx, gy)) = level.sample_gradient(q.0, q.1) else {
                continue;
            };
            let r = b - f64::from(a) - parameters.brightness;
            residuals.push(r);
            // dq/dθ = R'(θ)(p − c) in full-resolution pixels, over the level's scale.
            let (dx, dy) = (p.0 - transform.center_x, p.1 - transform.center_y);
            let dqx = (-s * dx - c * dy) / level.scale;
            let dqy = (c * dx - s * dy) / level.scale;
            let j = [
                gx * dqx + gy * dqy,
                gx / level.scale,
                gy / level.scale,
                -1.0,
            ];
            let w = if r.abs() <= k { 1.0 } else { k / r.abs() };
            for row in 0..4 {
                g[row] += w * j[row] * r;
                for column in 0..4 {
                    h[row][column] += w * j[row] * j[column];
                }
            }
        }
    }
    Normal { h, g, residuals }
}

/// Solve `h · x = g` by Gaussian elimination with partial pivoting; `None` when singular.
fn solve4(mut h: [[f64; 4]; 4], mut g: [f64; 4]) -> Option<[f64; 4]> {
    let scale = (0..4).map(|i| h[i][i].abs()).fold(0.0, f64::max);
    if scale <= 0.0 || !scale.is_finite() {
        return None;
    }
    for column in 0..4 {
        let pivot =
            (column..4).max_by(|&a, &b| h[a][column].abs().total_cmp(&h[b][column].abs()))?;
        if h[pivot][column].abs() <= 1e-12 * scale {
            return None;
        }
        h.swap(column, pivot);
        g.swap(column, pivot);
        let pivot_row = h[column];
        for row in column + 1..4 {
            let factor = h[row][column] / pivot_row[column];
            for (value, pivot) in h[row].iter_mut().zip(pivot_row).skip(column) {
                *value -= factor * pivot;
            }
            g[row] -= factor * g[column];
        }
    }
    let mut x = [0.0; 4];
    for row in (0..4).rev() {
        let mut sum = g[row];
        for k in row + 1..4 {
            sum -= h[row][k] * x[k];
        }
        x[row] = sum / h[row][row];
    }
    Some(x)
}

/// The smallest eigenvalue of the geometric normal matrix once the brightness is eliminated,
/// with the rotation expressed as the displacement it gives at the level's half-diagonal, in
/// (EV per level pixel)² summed over the samples: the information the weakest direction of motion
/// rests on.
fn weakest_direction(h: &[[f64; 4]; 4], level: &Level) -> f64 {
    if h[3][3] <= 0.0 {
        return 0.0;
    }
    let radius = ((level.width * level.width + level.height * level.height) as f64).sqrt() / 2.0;
    // Columns per level pixel of displacement: a radian moves the half-diagonal `radius` level
    // pixels, and a level pixel is `scale` full-resolution pixels.
    let unit = [1.0 / radius, level.scale, level.scale];
    let mut s = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            s[i][j] = (h[i][j] - h[i][3] * h[3][j] / h[3][3]) * unit[i] * unit[j];
        }
    }
    symmetric_eigenvalues(s)
        .into_iter()
        .fold(f64::INFINITY, f64::min)
        .max(0.0)
}

/// The eigenvalues of a symmetric 3 × 3 matrix by cyclic Jacobi rotations, a fixed number of
/// sweeps.
fn symmetric_eigenvalues(mut a: [[f64; 3]; 3]) -> [f64; 3] {
    for _ in 0..16 {
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() <= 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for row in &mut a {
                let (akp, akq) = (row[p], row[q]);
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            let (rp, rq) = (a[p], a[q]);
            a[p] = std::array::from_fn(|k| c * rp[k] - s * rq[k]);
            a[q] = std::array::from_fn(|k| s * rp[k] + c * rq[k]);
        }
    }
    [a[0][0], a[1][1], a[2][2]]
}
