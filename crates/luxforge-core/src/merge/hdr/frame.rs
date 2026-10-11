//! The HDR provider's typed frame contract: what each input says about itself
//! ([`FrameMetadata`]), the suitability check over a request ([`validate_bracket`]) and its typed
//! refusals ([`Refusal`]), and the borrowed per-rectangle pixel views fusion reads: an input's
//! aligned samples with their sensor-clipping evidence ([`FrameRect`], [`SensorLevel`]) and the
//! alignment's optional per-pixel motion selection ([`MotionSelection`]).
//!
//! Inputs are named by their position in the request; every refusal names the offending
//! position. Capture order is `captured_at_ms`, unrecorded last, then request position.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The fewest inputs a merge accepts.
pub const MIN_INPUTS: usize = 2;
/// The most inputs a merge accepts.
pub const MAX_INPUTS: usize = 9;

/// Two recorded f-numbers are the same setting within this relative difference: EXIF rationals of
/// one setting compare equal.
const SAME_SETTING: f64 = 1e-3;
/// A recorded focal length is the first input's within this many millimetres, or within
/// [`FOCAL_TOLERANCE_RELATIVE`] of it when that is more: a zoom records its focal length rounded,
/// often to whole millimetres, so frames of one unmoved zoom can record 60, 60 and 59 mm.
/// Registration is rigid, so a frame whose framing really differs is refused there.
pub const FOCAL_TOLERANCE_MM: f64 = 1.0;
/// The relative part of the focal-length tolerance: 2% of the first input's focal length.
pub const FOCAL_TOLERANCE_RELATIVE: f64 = 0.02;
/// Two recorded shutter times or biases are the same exposure within this relative (time) or
/// absolute (EV) difference.
const SAME_EXPOSURE: f64 = 1e-6;

/// The sensor arrangement a RAW recording mode develops from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SensorLayout {
    /// A 2×2 colour filter array, demosaiced by RCD, which clips its gained input at sensor
    /// white.
    Bayer,
    /// A 6×6 X-Trans array, demosaiced by Markesteijn, which keeps over-white input.
    XTrans,
    /// Already demosaiced linear camera RGB.
    LinearRgb,
    /// One channel without a colour filter array.
    Monochrome,
}

impl SensorLayout {
    /// The layout of a decoded RAW interpretation, or `None` for a mosaic whose array is neither
    /// Bayer nor X-Trans.
    pub fn of_raw(metadata: &luxforge_raw::RawMetadata) -> Option<Self> {
        match metadata.layout {
            luxforge_raw::RawLayout::LinearRgb => Some(Self::LinearRgb),
            luxforge_raw::RawLayout::Monochrome => Some(Self::Monochrome),
            luxforge_raw::RawLayout::Mosaic => match (metadata.cfa_width, metadata.cfa_height) {
                (2, 2) => Some(Self::Bayer),
                (6, 6) => Some(Self::XTrans),
                _ => None,
            },
        }
    }

    pub fn is_monochrome(self) -> bool {
        self == Self::Monochrome
    }
}

/// How an input was recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Recording {
    /// A supported RAW recording mode (the camera catalog's mode identifier) and its layout.
    Raw { mode: String, layout: SensorLayout },
    /// A JPEG: browsable and editable, never an HDR input.
    Jpeg,
}

/// An input's recorded exposure settings. Unrecorded or invalid values are `None`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExposureSettings {
    pub shutter_s: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    /// The recorded exposure bias (compensation) in EV.
    pub bias_ev: Option<f64>,
    pub focal_mm: Option<f64>,
}

impl ExposureSettings {
    /// The shutter time when it is finite and positive.
    pub(crate) fn usable_shutter(&self) -> Option<f64> {
        self.shutter_s
            .filter(|time| time.is_finite() && *time > 0.0)
    }

    /// The ISO when it is positive.
    pub(crate) fn usable_iso(&self) -> Option<u32> {
        self.iso.filter(|iso| *iso > 0)
    }

    /// The bias when it is finite.
    pub(crate) fn usable_bias(&self) -> Option<f64> {
        self.bias_ev.filter(|bias| bias.is_finite())
    }
}

/// What one input says about itself: everything suitability, exposure resolution and provenance
/// read. The merge job fills it from the verified source and its capture metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameMetadata {
    /// A stable token naming the exact input (its source fingerprint or catalog identity); two
    /// inputs with one identity are the same file.
    pub identity: String,
    pub make: Option<String>,
    pub model: Option<String>,
    pub recording: Recording,
    /// Upright width and height, after the default crop and orientation.
    pub width: u32,
    pub height: u32,
    /// The EXIF orientation, 1 to 8, that turned the sensor frame upright.
    pub orientation: u8,
    pub exposure: ExposureSettings,
    /// The capture instant in milliseconds, when recorded.
    pub captured_at_ms: Option<i64>,
}

impl FrameMetadata {
    fn is_raw(&self) -> bool {
        matches!(self.recording, Recording::Raw { .. })
    }
}

/// Why a request cannot be merged. Each reason names the offending input position(s) of the
/// request; `first` is always the earlier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Refusal {
    /// Fewer than [`MIN_INPUTS`] or more than [`MAX_INPUTS`] inputs.
    InputCount {
        count: usize,
    },
    /// One identity named twice.
    RepeatedInput {
        first: usize,
        second: usize,
    },
    /// A RAW and a JPEG of the same exposure: alternative records of one frame, not two bracket
    /// frames.
    SameExposure {
        first: usize,
        second: usize,
    },
    /// A JPEG input: merges are RAW only.
    RawOnly {
        input: usize,
    },
    MixedCameras {
        input: usize,
    },
    MixedRecordingModes {
        input: usize,
    },
    /// Monochrome and colour recordings together.
    MixedColour {
        input: usize,
    },
    MixedDimensions {
        input: usize,
    },
    MixedAperture {
        input: usize,
    },
    /// A focal length further from the first input's than [`FOCAL_TOLERANCE_MM`] or
    /// [`FOCAL_TOLERANCE_RELATIVE`] allows, or recorded on one and not the other.
    MixedFocalLength {
        input: usize,
    },
    /// Two inputs whose effective exposures are less than the minimum step apart.
    IndistinctExposures {
        first: usize,
        second: usize,
    },
    /// An effective offset beyond the supported range from the reference.
    ExposureOutOfRange {
        input: usize,
    },
    /// A requested reference outside the request.
    InvalidReference {
        reference: usize,
    },
    /// Overrides that do not name every input once.
    OverrideCount {
        count: usize,
        inputs: usize,
    },
    /// A non-finite or out-of-range override, or a nonzero override on the reference.
    InvalidOverride {
        input: usize,
    },
    /// Overrides are offsets from the reference, but nothing establishes it: name the reference,
    /// give it a zero override, or record usable exposure metadata.
    OverridesNeedReference,
    /// Too few usable aligned samples connect this input to the reference for a measured fit.
    InsufficientOverlap {
        input: usize,
    },
    /// The measured offset is ambiguous: its samples disagree by `spread_ev` (robust deviation)
    /// or the pairwise solution leaves `residual_ev`.
    AmbiguousExposure {
        input: usize,
        spread_ev: f64,
        residual_ev: f64,
    },
    /// Too few samples usable in both this input and the reference connect them for alignment:
    /// the frames barely overlap, or their exposures share too little usable range.
    AlignmentSupport {
        input: usize,
        samples: u64,
    },
    /// The scene has too little texture to fix this input's position in some direction.
    LowTexture {
        input: usize,
    },
    /// No consistent alignment: the best match correlates by only `correlation`, another
    /// distinct match correlates nearly as well, or after the fit only `inlier_fraction` of the
    /// shared samples agree with it.
    AmbiguousAlignment {
        input: usize,
        correlation: f64,
        inlier_fraction: Option<f64>,
    },
    /// The input moved further than handheld alignment corrects: `shift_px` full-resolution
    /// pixels and `rotation_deg` degrees from the reference.
    SevereMisalignment {
        input: usize,
        shift_px: f64,
        rotation_deg: f64,
    },
    /// Alignment is off but the input is not framed as the reference: another orientation
    /// (`shift_px` absent), or a visible displacement of `shift_px` full-resolution pixels.
    FramingMismatch {
        input: usize,
        shift_px: Option<f64>,
    },
    /// The aligned inputs share only a `width × height` rectangle, under half the frame on a
    /// side.
    InsufficientCoverage {
        width: u32,
        height: u32,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = |input: &usize| input + 1;
        match self {
            Self::InputCount { count } => write!(
                f,
                "an HDR merge takes {MIN_INPUTS} to {MAX_INPUTS} exposures, not {count}"
            ),
            Self::RepeatedInput { first, second } => {
                write!(f, "inputs {} and {} are the same file", n(first), n(second))
            }
            Self::SameExposure { first, second } => write!(
                f,
                "inputs {} and {} are a RAW and a JPEG of one exposure",
                n(first),
                n(second)
            ),
            Self::RawOnly { input } => write!(
                f,
                "input {} is a JPEG; HDR merges take RAW exposures only",
                n(input)
            ),
            Self::MixedCameras { input } => {
                write!(f, "input {} is from a different camera", n(input))
            }
            Self::MixedRecordingModes { input } => {
                write!(f, "input {} uses a different RAW recording mode", n(input))
            }
            Self::MixedColour { input } => {
                write!(f, "input {} mixes monochrome and colour", n(input))
            }
            Self::MixedDimensions { input } => {
                write!(f, "input {} has different dimensions", n(input))
            }
            Self::MixedAperture { input } => {
                write!(f, "input {} uses a different aperture", n(input))
            }
            Self::MixedFocalLength { input } => {
                write!(f, "input {} uses a different focal length", n(input))
            }
            Self::IndistinctExposures { first, second } => write!(
                f,
                "inputs {} and {} have the same effective exposure",
                n(first),
                n(second)
            ),
            Self::ExposureOutOfRange { input } => write!(
                f,
                "input {} is too far in exposure from the reference",
                n(input)
            ),
            Self::InvalidReference { reference } => {
                write!(f, "reference {} is not one of the inputs", n(reference))
            }
            Self::OverrideCount { count, inputs } => write!(
                f,
                "{count} exposure overrides were given for {inputs} inputs"
            ),
            Self::InvalidOverride { input } => {
                write!(f, "the exposure override of input {} is invalid", n(input))
            }
            Self::OverridesNeedReference => f.write_str(
                "exposure overrides need a reference: name one, give it a 0 EV override, or \
                 record exposure metadata",
            ),
            Self::InsufficientOverlap { input } => write!(
                f,
                "input {} shares too few usable samples to measure its exposure",
                n(input)
            ),
            Self::AmbiguousExposure {
                input,
                spread_ev,
                residual_ev,
            } => write!(
                f,
                "the measured exposure of input {} is ambiguous (spread {spread_ev:.3} EV, \
                 residual {residual_ev:.3} EV)",
                n(input)
            ),
            Self::AlignmentSupport { input, samples } => write!(
                f,
                "input {} shares too few usable samples with the reference to align it \
                 ({samples})",
                n(input)
            ),
            Self::LowTexture { input } => write!(
                f,
                "input {} has too little detail to align with the reference",
                n(input)
            ),
            Self::AmbiguousAlignment {
                input,
                correlation,
                inlier_fraction,
            } => match inlier_fraction {
                Some(inliers) => write!(
                    f,
                    "input {} has no consistent alignment with the reference (only {:.0}% of \
                     samples agree)",
                    n(input),
                    inliers * 100.0
                ),
                None => write!(
                    f,
                    "input {} has no consistent alignment with the reference (correlation \
                     {correlation:.2})",
                    n(input)
                ),
            },
            Self::SevereMisalignment {
                input,
                shift_px,
                rotation_deg,
            } => write!(
                f,
                "input {} moved too far to align ({shift_px:.0} px, {rotation_deg:.1}°)",
                n(input)
            ),
            Self::FramingMismatch { input, shift_px } => match shift_px {
                Some(shift) => write!(
                    f,
                    "input {} is displaced by about {shift:.0} px; turn alignment on to merge it",
                    n(input)
                ),
                None => write!(
                    f,
                    "input {} has a different orientation; turn alignment on to merge it",
                    n(input)
                ),
            },
            Self::InsufficientCoverage { width, height } => write!(
                f,
                "the aligned exposures share only a {width} × {height} area"
            ),
        }
    }
}

impl From<Refusal> for Error {
    /// A refusal is the request's fault: RAW-only is an unsupported input, everything else a
    /// validation failure. `data` carries the typed refusal.
    fn from(refusal: Refusal) -> Self {
        let error = match refusal {
            Refusal::RawOnly { .. } => Error::unsupported_input(refusal.to_string()),
            _ => Error::validation(refusal.to_string()),
        };
        match serde_json::to_value(&refusal) {
            Ok(data) => error.with_data(data),
            Err(_) => error,
        }
    }
}

/// Positions in capture order: by recorded instant, unrecorded last, then request position.
pub(crate) fn capture_order(frames: &[FrameMetadata]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.sort_by_key(|&index| {
        let at = frames[index].captured_at_ms;
        (at.is_none(), at, index)
    });
    order
}

fn same_option(a: Option<f64>, b: Option<f64>, same: impl Fn(f64, f64) -> bool) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => same(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn same_relative(tolerance: f64) -> impl Fn(f64, f64) -> bool {
    move |a, b| (a - b).abs() <= tolerance * a.abs().max(b.abs())
}

/// Whether `focal` is the same focal length as the first input's `first`: within
/// [`FOCAL_TOLERANCE_MM`] or [`FOCAL_TOLERANCE_RELATIVE`] of it, whichever is more.
fn same_focal(focal: f64, first: f64) -> bool {
    (focal - first).abs() <= FOCAL_TOLERANCE_MM.max(FOCAL_TOLERANCE_RELATIVE * first.abs())
}

/// Whether two inputs of different recordings are one exposure: one body, one recorded instant
/// and the same shutter, aperture, ISO and bias. Without a recorded instant this cannot be told,
/// and such frames are not called duplicates.
fn same_exposure(a: &FrameMetadata, b: &FrameMetadata) -> bool {
    let (ea, eb) = (&a.exposure, &b.exposure);
    a.make == b.make
        && a.model == b.model
        && a.captured_at_ms.is_some()
        && a.captured_at_ms == b.captured_at_ms
        && same_option(ea.shutter_s, eb.shutter_s, same_relative(SAME_EXPOSURE))
        && same_option(ea.f_number, eb.f_number, same_relative(SAME_SETTING))
        && ea.iso == eb.iso
        && same_option(ea.bias_ev, eb.bias_ev, |a, b| {
            (a - b).abs() <= SAME_EXPOSURE
        })
}

/// Whether `frames` can be merged at all, before any exposure is resolved: the input count, no
/// repeated file, no RAW+JPEG pair of one exposure, RAW only, then one body, recording mode,
/// colour interpretation, upright dimensions, aperture and focal length (within
/// [`FOCAL_TOLERANCE_MM`] or [`FOCAL_TOLERANCE_RELATIVE`]), each compared with the first input. An
/// unrecorded value differs from a recorded one; inputs that all lack a value (an unknown body, a
/// manual lens) agree. ISO may vary: auto ISO changes it within a camera's bracket, and exposure
/// resolution counts it as gain ([`super::MetadataExposure`]).
pub fn validate_bracket(frames: &[FrameMetadata]) -> Result<(), Refusal> {
    if !(MIN_INPUTS..=MAX_INPUTS).contains(&frames.len()) {
        return Err(Refusal::InputCount {
            count: frames.len(),
        });
    }
    for (first, a) in frames.iter().enumerate() {
        for (second, b) in frames.iter().enumerate().skip(first + 1) {
            if a.identity == b.identity {
                return Err(Refusal::RepeatedInput { first, second });
            }
            if a.is_raw() != b.is_raw() && same_exposure(a, b) {
                return Err(Refusal::SameExposure { first, second });
            }
        }
    }
    if let Some(input) = frames.iter().position(|frame| !frame.is_raw()) {
        return Err(Refusal::RawOnly { input });
    }
    let first = &frames[0];
    let Recording::Raw {
        mode: first_mode,
        layout: first_layout,
    } = &first.recording
    else {
        unreachable!("every input is RAW");
    };
    for (input, frame) in frames.iter().enumerate().skip(1) {
        let Recording::Raw { mode, layout } = &frame.recording else {
            unreachable!("every input is RAW");
        };
        let (e, e0) = (&frame.exposure, &first.exposure);
        if frame.make != first.make || frame.model != first.model {
            return Err(Refusal::MixedCameras { input });
        }
        if mode != first_mode {
            return Err(Refusal::MixedRecordingModes { input });
        }
        if layout.is_monochrome() != first_layout.is_monochrome() {
            return Err(Refusal::MixedColour { input });
        }
        if (frame.width, frame.height) != (first.width, first.height) {
            return Err(Refusal::MixedDimensions { input });
        }
        if !same_option(e.f_number, e0.f_number, same_relative(SAME_SETTING)) {
            return Err(Refusal::MixedAperture { input });
        }
        if !same_option(e.focal_mm, e0.focal_mm, same_focal) {
            return Err(Refusal::MixedFocalLength { input });
        }
    }
    Ok(())
}

/// One sample's sensor-clipping evidence: its largest camera channel as a fraction of that
/// channel's sensor ceiling in the development ([`super::SensorClipModel`]), quantized down to
/// 1/32768, or [`Self::CLIPPED`] at or above [`Self::CLIP_FRACTION`]. Two bytes per sample beside
/// the twelve of its RGB.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SensorLevel(pub u16);

impl SensorLevel {
    /// Codes per unit fraction.
    pub const SCALE: f64 = 32768.0;
    /// A fraction at or above this is recorded as clipped: an exactly saturated site reads within
    /// about `1e-5` of 1 through RCD's `65536/65535` ceiling and the f32 calibration round trip.
    pub const CLIP_FRACTION: f64 = 0.999;
    /// The sensor recorded no measurement here: some camera channel reached its ceiling.
    pub const CLIPPED: Self = Self(u16::MAX);

    /// The level of `fraction`: zero at or below zero, [`Self::CLIPPED`] at or above
    /// [`Self::CLIP_FRACTION`] or when not a number, otherwise `floor(fraction · 32768)`.
    pub fn from_fraction(fraction: f64) -> Self {
        if fraction.is_nan() || fraction >= Self::CLIP_FRACTION {
            Self::CLIPPED
        } else if fraction <= 0.0 {
            Self(0)
        } else {
            Self((fraction * Self::SCALE) as u16)
        }
    }

    pub fn is_clipped(self) -> bool {
        self == Self::CLIPPED
    }

    /// The quantized fraction, `code / 32768`; 1 when clipped.
    pub fn fraction(self) -> f64 {
        if self.is_clipped() {
            1.0
        } else {
            f64::from(self.0) / Self::SCALE
        }
    }
}

/// The smallest slice length a `width × height` view with `stride` needs.
pub(crate) fn view_len(width: usize, height: usize, stride: usize) -> Option<usize> {
    if height == 0 || width == 0 {
        return Some(0);
    }
    (height - 1).checked_mul(stride)?.checked_add(width)
}

/// One input's aligned samples for one rectangle in reference coordinates: three float32 linear
/// sRGB/D65 planes and the samples' [`SensorLevel`]s, all `width × height` at one row `stride`.
/// The samples are already resampled into reference coordinates; a level that alignment
/// resampled takes the largest level its footprint read, so clipping evidence is never diluted.
#[derive(Clone, Copy, Debug)]
pub struct FrameRect<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub rgb: [&'a [f32]; 3],
    pub levels: &'a [SensorLevel],
}

impl FrameRect<'_> {
    /// Refuse a view whose slices are shorter than its dimensions, or whose stride is narrower
    /// than a row.
    pub(crate) fn check(&self) -> Result<(), Error> {
        let need = view_len(self.width, self.height, self.stride)
            .filter(|_| self.stride >= self.width)
            .ok_or_else(|| {
                Error::validation("HDR frame rectangle stride is narrower than a row")
            })?;
        if self.rgb.iter().any(|plane| plane.len() < need) || self.levels.len() < need {
            return Err(Error::validation(
                "HDR frame rectangle slices are shorter than its dimensions",
            ));
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn at(&self, x: usize, y: usize) -> ([f32; 3], SensorLevel) {
        let i = y * self.stride + x;
        (
            [self.rgb[0][i], self.rgb[1][i], self.rgb[2][i]],
            self.levels[i],
        )
    }
}

/// The alignment's motion decision for one rectangle: at each pixel the input chosen to stand
/// alone in a moving region and the confidence, in `[0, 1]`, that the pixel is moving. Fusion
/// feathers `(1 − c) · fused + c · normalized(selected)`; a zero confidence leaves the pixel
/// fused. Both slices are `width × height` at `stride`.
#[derive(Clone, Copy, Debug)]
pub struct MotionSelection<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub selected: &'a [u8],
    pub confidence: &'a [f32],
}

impl MotionSelection<'_> {
    pub(crate) fn check(&self, inputs: usize) -> Result<(), Error> {
        let need = view_len(self.width, self.height, self.stride)
            .filter(|_| self.stride >= self.width)
            .ok_or_else(|| Error::validation("motion selection stride is narrower than a row"))?;
        if self.selected.len() < need || self.confidence.len() < need {
            return Err(Error::validation(
                "motion selection slices are shorter than its dimensions",
            ));
        }
        for y in 0..self.height {
            for x in 0..self.width {
                let i = y * self.stride + x;
                if usize::from(self.selected[i]) >= inputs {
                    return Err(Error::validation("motion selection names a missing input"));
                }
                if !(0.0..=1.0).contains(&self.confidence[i]) {
                    return Err(Error::validation(
                        "motion selection confidence is outside [0, 1]",
                    ));
                }
            }
        }
        Ok(())
    }
}
