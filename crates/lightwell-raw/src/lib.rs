//! Pinned, bounded RAW unpacking and float development adapter.
//!
//! The native boundary is private to this crate. Source ownership, limits, and
//! pointer lifetimes are enforced by the safe public API.

use serde::{Deserialize, Serialize};
use std::{
    ffi::{c_char, c_int, c_void},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
mod dng;
mod dng_ops;
mod format;
mod limits;
mod native_tiles;
mod profiles;
pub use dng::{DngCalibrationMetadata, DngCorrectionMetadata, DngOpcodeProvenance};
pub use format::required_dng_opcodes;
use format::{classify_mode, raf_default_crop};
pub use limits::{MAX_PIXELS, MAX_RGB_BYTES, MAX_SIDE, MAX_SOURCE_BYTES};
use profiles::{Catalog, Crop};

fn camera_catalog() -> &'static Catalog {
    static CATALOG: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        Catalog::parse(include_str!("../data/cameras.json"))
            .expect("camera catalog validated at build time")
    })
}

const PROVIDER: &str = "LibRaw 0.22.2 + librtprocess 9a858270";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawError {
    InvalidInput(&'static str),
    UnsupportedMode(String),
    UnsupportedRequiredOpcodes(Vec<u32>),
    UnsupportedCfa,
    MissingCalibration(&'static str),
    ResourceLimit(&'static str),
    Cancelled,
    Native(String),
}

impl fmt::Display for RawError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(v) => write!(f, "invalid RAW input: {v}"),
            Self::UnsupportedMode(v) => write!(f, "unsupported RAW recording mode: {v}"),
            Self::UnsupportedRequiredOpcodes(ids) => {
                write!(f, "unsupported mandatory DNG opcodes: {ids:?}")
            }
            Self::UnsupportedCfa => write!(f, "unsupported RAW color filter array"),
            Self::MissingCalibration(v) => write!(f, "missing or invalid RAW calibration: {v}"),
            Self::ResourceLimit(v) => write!(f, "RAW resource limit: {v}"),
            Self::Cancelled => write!(f, "RAW work cancelled"),
            Self::Native(v) => write!(f, "RAW native decoder: {v}"),
        }
    }
}
impl std::error::Error for RawError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

include!(concat!(env!("OUT_DIR"), "/raw_modes.rs"));

impl RawMode {
    /// The correction record is required by the mode's processing capability.
    pub fn requires_dng_corrections(self) -> bool {
        camera_catalog().cameras.iter().any(|camera| {
            camera.dng.is_some() && camera.modes.iter().any(|mode| mode.id == self.id())
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawMetadata {
    pub make: String,
    pub model: String,
    pub mode: RawMode,
    pub sensor_width: u32,
    pub sensor_height: u32,
    pub active_area: RawRect,
    pub default_crop: RawRect,
    pub cfa_width: u8,
    pub cfa_height: u8,
    pub cfa: Vec<u8>,
    pub black_cfa: Vec<u8>,
    pub black_base: f32,
    pub black_channels: [f32; 4],
    pub black_repeat_width: u8,
    pub black_repeat_height: u8,
    pub black_repeat: Vec<f32>,
    pub sensor_white: f32,
    pub as_shot_gains: [f32; 3],
    pub libraw_flip: u8,
    pub rgb_cam: [[f32; 4]; 3],
    pub cam_xyz: [[f32; 3]; 4],
    pub backend: String,
    pub exif_orientation: u8,
    pub libraw_inset: Option<RawRect>,
    pub format_identity: String,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dng_corrections: Option<DngCorrectionMetadata>,
}

/// Sparse sensor repairs applied during development while retaining the exact
/// decoded mosaic. Sorted unique offsets; at most 65,536 entries per source.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MosaicCorrection {
    pub index: u32,
    pub value: u16,
}

#[derive(Debug, Clone)]
pub struct RawSource {
    metadata: RawMetadata,
    mosaic: Arc<Vec<u16>>,
    mosaic_corrections: Arc<Vec<MosaicCorrection>>,
    native: Box<NativeMetadata>,
    dng_correction: Option<dng::DngCorrection>,
}

#[derive(Debug)]
pub struct PlanarRgb {
    pub width: u32,
    pub height: u32,
    /// One contiguous [red plane, green plane, blue plane] allocation.
    pub data: Vec<f32>,
}

fn validate_camera_response(matrix: &[[f32; 3]; 4]) -> Result<(), RawError> {
    // LibRaw can unpack an uncalibrated camera with an identity rgb_cam and
    // zero cam_xyz. Finite development alone would then silently use wrong
    // display colors and leave temperature/tint unusable.
    if matrix
        .iter()
        .flatten()
        .any(|v| !v.is_finite() || v.abs() > 16.0)
        || matrix[3] != [0.0; 3]
    {
        return Err(RawError::MissingCalibration("XYZ-to-camera response"));
    }
    let m = matrix.map(|row| row.map(f64::from));
    let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !determinant.is_finite() || determinant.abs() < 1e-8 {
        return Err(RawError::MissingCalibration(
            "singular XYZ-to-camera response",
        ));
    }
    Ok(())
}

impl PlanarRgb {
    pub fn plane_len(&self) -> usize {
        self.width as usize * self.height as usize
    }
    pub fn planes(&self) -> (&[f32], &[f32], &[f32]) {
        let n = self.plane_len();
        let (red, rest) = self.data.split_at(n);
        let (green, blue) = rest.split_at(n);
        (red, green, blue)
    }
}

#[repr(C)]
#[derive(Debug, Clone)]
struct NativeMetadata {
    make: [c_char; 64],
    model: [c_char; 64],
    decoder: [c_char; 80],
    width: u32,
    height: u32,
    raw_pitch: u32,
    raw_bps: u32,
    dng_version: u32,
    decoder_flags: u32,
    active_x: u32,
    active_y: u32,
    active_width: u32,
    active_height: u32,
    inset_x: u32,
    inset_y: u32,
    inset_width: u32,
    inset_height: u32,
    cfa_width: u32,
    cfa_height: u32,
    flip: u32,
    raw_count: u32,
    cfa: [u8; 36],
    black_cfa: [u8; 36],
    black_base: f32,
    black_channels: [f32; 4],
    black_repeat_width: u32,
    black_repeat_height: u32,
    black_repeat: [f32; 4096],
    white: f32,
    as_shot: [f32; 3],
    rgb_cam: [f32; 12],
    cam_xyz: [f32; 12],
}

type CancelCallback = extern "C" fn(*mut c_void) -> c_int;
unsafe extern "C" {
    fn lw_raw_open(
        bytes: *const u8,
        length: usize,
        cancel: CancelCallback,
        cancel_context: *mut c_void,
        handle_out: *mut *mut c_void,
        meta: *mut NativeMetadata,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn lw_raw_copy(
        handle: *mut c_void,
        dest: *mut u16,
        length: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn lw_raw_close(handle: *mut c_void);
    fn lw_raw_develop(
        samples: *const u16,
        count: usize,
        meta: *const NativeMetadata,
        patches: *const MosaicCorrection,
        patch_count: usize,
        gains: *const f32,
        red: *mut f32,
        green: *mut f32,
        blue: *mut f32,
        test_fault: u32,
        executor: Option<native_tiles::TileExecutor>,
        executor_context: *mut c_void,
        cancel: CancelCallback,
        cancel_context: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

extern "C" fn cancelled(context: *mut c_void) -> c_int {
    // SAFETY: every native call receives a pointer to the caller's live
    // AtomicBool, invokes this synchronously, and never stores the pointer.
    let token = unsafe { &*(context.cast::<AtomicBool>()) };
    c_int::from(token.load(Ordering::Relaxed))
}

struct NativeHandle(*mut c_void);
impl Drop for NativeHandle {
    fn drop(&mut self) {
        // SAFETY: the pointer is the unique handle returned by lw_raw_open;
        // close accepts null and is called exactly once by this guard.
        unsafe { lw_raw_close(self.0) };
    }
}

fn c_text(chars: &[c_char]) -> String {
    let end = chars.iter().position(|&v| v == 0).unwrap_or(chars.len());
    String::from_utf8_lossy(&chars[..end].iter().map(|&v| v as u8).collect::<Vec<_>>()).into_owned()
}

fn native_error(code: c_int, buffer: &[c_char]) -> RawError {
    match code {
        2 => RawError::Cancelled,
        4 | 6 => RawError::ResourceLimit("native decoder allocation or geometry"),
        5 => RawError::UnsupportedCfa,
        7 => RawError::UnsupportedMode(c_text(buffer)),
        _ => RawError::Native(c_text(buffer)),
    }
}

fn reject_unhandled_required_opcodes(
    handles_dng_corrections: bool,
    opcodes: &[format::DngOpcode],
) -> Result<(), RawError> {
    if !handles_dng_corrections {
        let mut ids = opcodes
            .iter()
            .filter(|op| op.flags & 1 == 0)
            .map(|op| op.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        if !ids.is_empty() {
            return Err(RawError::UnsupportedRequiredOpcodes(ids));
        }
    }
    Ok(())
}

fn checked_rect(rect: RawRect, width: u32, height: u32) -> Result<RawRect, RawError> {
    if rect.width == 0
        || rect.height == 0
        || rect.x.checked_add(rect.width).is_none_or(|v| v > width)
        || rect.y.checked_add(rect.height).is_none_or(|v| v > height)
    {
        return Err(RawError::InvalidInput("crop outside sensor"));
    }
    Ok(rect)
}

fn libraw_inset(
    native: &NativeMetadata,
    width: u32,
    height: u32,
) -> Result<Option<RawRect>, RawError> {
    let rect = RawRect {
        x: native.inset_x,
        y: native.inset_y,
        width: native.inset_width,
        height: native.inset_height,
    };
    // LibRaw uses an empty size, or a 65535,65535 origin sentinel (while
    // retaining the reported size), when no inset was supplied. Any other
    // malformed/non-empty rectangle must fail explicitly.
    if (rect.width == 0 && rect.height == 0) || (rect.x == 65_535 && rect.y == 65_535) {
        return Ok(None);
    }
    checked_rect(rect, width, height).map(Some)
}

fn exif_orientation(libraw_flip: u32) -> Result<u8, RawError> {
    const EXIF: [u8; 8] = [1, 2, 4, 3, 5, 8, 6, 7];
    EXIF.get(libraw_flip as usize)
        .copied()
        .ok_or(RawError::InvalidInput("orientation"))
}

impl RawSource {
    pub fn metadata(&self) -> &RawMetadata {
        &self.metadata
    }
    pub fn mosaic(&self) -> &[u16] {
        &self.mosaic
    }
    pub fn mosaic_arc(&self) -> Arc<Vec<u16>> {
        Arc::clone(&self.mosaic)
    }

    /// Decode once on a bounded worker. The original byte buffer is dropped
    /// after an owned u16 sensor mosaic is copied and the native handle closes.
    pub fn decode(bytes: Arc<[u8]>, cancel: &AtomicBool) -> Result<Self, RawError> {
        if bytes.is_empty() {
            return Err(RawError::InvalidInput("empty source"));
        }
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(RawError::ResourceLimit("source exceeds 512 MiB"));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let opcodes = if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
            let found = format::dng_opcodes(&bytes)?;
            let mut unknown: Vec<_> = found
                .iter()
                .filter(|op| {
                    op.flags & 1 == 0
                        && !matches!((op.list, op.id), (51022, 1 | 3 | 9) | (51008, 4 | 5))
                })
                .map(|op| op.id)
                .collect();
            unknown.sort_unstable();
            unknown.dedup();
            if !unknown.is_empty() {
                return Err(RawError::UnsupportedRequiredOpcodes(unknown));
            }
            found
        } else {
            Vec::new()
        };
        let mut native = Box::new(Self::blank_native());
        let mut handle = std::ptr::null_mut();
        let mut error = [0 as c_char; 256];
        // SAFETY: Arc pins bytes for this synchronous call and until handle is
        // closed. Metadata and error are writable, and token lives for call.
        let code = unsafe {
            lw_raw_open(
                bytes.as_ptr(),
                bytes.len(),
                cancelled,
                (cancel as *const AtomicBool).cast_mut().cast(),
                &mut handle,
                &mut *native,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code != 0 {
            return Err(native_error(code, &error));
        }
        let guard = NativeHandle(handle);
        let n = Self::checked_len(&native)?;
        let (mut metadata, dng_correction) = Self::interpret(&native, &bytes, &opcodes)?;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(n)
            .map_err(|_| RawError::ResourceLimit("sensor mosaic allocation"))?;
        samples.resize(n, 0);
        // SAFETY: guard owns the live LibRaw handle; samples has exactly n
        // initialized u16 slots and native copy validates dimensions.
        let code = unsafe {
            lw_raw_copy(
                guard.0,
                samples.as_mut_ptr(),
                n,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code != 0 {
            return Err(native_error(code, &error));
        }
        drop(guard);
        drop(bytes);
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let (mosaic_corrections, unresolved) = match &dng_correction {
            Some(correction) => correction.mosaic_corrections(
                &samples,
                native.width as usize,
                native.height as usize,
                &metadata.cfa,
            )?,
            None => (Vec::new(), 0),
        };
        if unresolved > 0 {
            metadata.warnings.push(format!("DNG bad-pixel interpolation left {unresolved} markers unchanged because no usable same-color neighbors were available"));
        }
        Ok(Self {
            metadata,
            mosaic_corrections: Arc::new(mosaic_corrections),
            mosaic: Arc::new(samples),
            native,
            dng_correction,
        })
    }

    pub fn mosaic_corrections(&self) -> &[MosaicCorrection] {
        &self.mosaic_corrections
    }

    /// Demosaic with green-normalized camera-channel gains. These gains are
    /// applied to black-subtracted, sensor-white-normalized samples *before*
    /// the pinned demosaicer; changing WB reruns this stage from the mosaic.
    pub fn develop(&self, gains: [f32; 3], cancel: &AtomicBool) -> Result<PlanarRgb, RawError> {
        let mut image = self.develop_uncorrected(gains, cancel)?;
        if let Some(correction) = &self.dng_correction {
            correction.apply(&mut image, cancel)?;
        }
        Ok(image)
    }

    fn develop_uncorrected(
        &self,
        gains: [f32; 3],
        cancel: &AtomicBool,
    ) -> Result<PlanarRgb, RawError> {
        self.develop_uncorrected_with_workers(gains, cancel, 0)
    }

    fn develop_uncorrected_with_workers(
        &self,
        gains: [f32; 3],
        cancel: &AtomicBool,
        worker_limit: usize,
    ) -> Result<PlanarRgb, RawError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        if !gains
            .iter()
            .all(|v| v.is_finite() && *v > 0.0 && *v <= 32.0)
            || (gains[1] - 1.0).abs() > 1e-6
        {
            return Err(RawError::InvalidInput(
                "WB gains must be finite, positive, green-normalized, <=32",
            ));
        }
        let n = self.mosaic.len();
        let rgb_bytes = n
            .checked_mul(3)
            .and_then(|v| v.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(RawError::ResourceLimit("RGB allocation overflow"))?;
        if rgb_bytes > MAX_RGB_BYTES {
            return Err(RawError::ResourceLimit("RGB planes exceed 1.5 GiB"));
        }
        let mut data = Vec::new();
        data.try_reserve_exact(n * 3)
            .map_err(|_| RawError::ResourceLimit("RGB plane allocation"))?;
        data.resize(n * 3, 0.0);
        let (red, rest) = data.split_at_mut(n);
        let (green, blue) = rest.split_at_mut(n);
        let mut error = [0 as c_char; 256];
        let mut executor_context = native_tiles::ExecutorContext {
            cancel,
            worker_limit,
        };
        // SAFETY: immutable mosaic/meta and three disjoint initialized planes
        // stay alive for the synchronous call; C++ validates count, catches
        // exceptions, and stores none of these pointers.
        let code = unsafe {
            lw_raw_develop(
                self.mosaic.as_ptr(),
                n,
                &*self.native,
                self.mosaic_corrections.as_ptr(),
                self.mosaic_corrections.len(),
                gains.as_ptr(),
                red.as_mut_ptr(),
                green.as_mut_ptr(),
                blue.as_mut_ptr(),
                0,
                Some(native_tiles::execute),
                (&mut executor_context as *mut native_tiles::ExecutorContext<'_>).cast(),
                cancelled,
                (cancel as *const AtomicBool).cast_mut().cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code != 0 {
            return Err(native_error(code, &error));
        }
        Ok(PlanarRgb {
            width: self.metadata.sensor_width,
            height: self.metadata.sensor_height,
            data,
        })
    }

    /// Map an absolute corrected-plane pixel to the uncorrected sensor
    /// coordinate used by the selected camera-color channel. Bounded point
    /// math only; this never develops or renders an image.
    pub fn corrected_sensor_sample_location(
        &self,
        x: u32,
        y: u32,
        channel: usize,
    ) -> Result<(f64, f64), RawError> {
        if channel >= 3 || x >= self.metadata.sensor_width || y >= self.metadata.sensor_height {
            return Err(RawError::InvalidInput("RAW corrected point outside sensor"));
        }
        match &self.dng_correction {
            Some(correction) => correction.source_location(x, y, channel),
            None => Ok((x as f64, y as f64)),
        }
    }

    /// Combined opcode gain at an absolute corrected output coordinate.
    /// Gain placement before or after a warp follows the source opcode order.
    pub fn gain_at_corrected_sensor(
        &self,
        x: f64,
        y: f64,
        channel: usize,
    ) -> Result<f64, RawError> {
        if channel >= 3
            || !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= self.metadata.sensor_width as f64
            || y >= self.metadata.sensor_height as f64
        {
            return Err(RawError::InvalidInput("RAW gain point outside sensor"));
        }
        match &self.dng_correction {
            Some(correction) => correction.gain_at(x, y, channel),
            None => Ok(1.0),
        }
    }

    fn blank_native() -> NativeMetadata {
        // SAFETY: this repr(C) POD contains only integers, floats, and arrays;
        // all-zero is a valid initialized value for every field.
        unsafe { std::mem::zeroed() }
    }

    fn checked_len(native: &NativeMetadata) -> Result<usize, RawError> {
        if native.width == 0
            || native.height == 0
            || native.width > MAX_SIDE
            || native.height > MAX_SIDE
        {
            return Err(RawError::ResourceLimit("sensor dimension"));
        }
        let n = (native.width as usize)
            .checked_mul(native.height as usize)
            .ok_or(RawError::ResourceLimit("sensor area overflow"))?;
        if n > MAX_PIXELS {
            return Err(RawError::ResourceLimit("sensor exceeds 128 MP"));
        }
        Ok(n)
    }

    fn interpret(
        native: &NativeMetadata,
        bytes: &[u8],
        opcodes: &[format::DngOpcode],
    ) -> Result<(RawMetadata, Option<dng::DngCorrection>), RawError> {
        let make = c_text(&native.make);
        let model = c_text(&native.model);
        let decoder = c_text(&native.decoder);
        let (profile, recording) =
            classify_mode(camera_catalog(), native, &make, &model, &decoder, bytes)?;
        let mode = serde_json::from_value(serde_json::Value::String(recording.id.clone()))
            .expect("mode identifiers generated from the validated catalog");
        reject_unhandled_required_opcodes(profile.dng.is_some(), opcodes)?;
        let rect = |x, y, width, height| RawRect {
            x,
            y,
            width,
            height,
        };
        let active = checked_rect(
            rect(
                native.active_x,
                native.active_y,
                native.active_width,
                native.active_height,
            ),
            native.width,
            native.height,
        )?;
        let inset = libraw_inset(native, native.width, native.height)?;
        let (cfa_w, cfa_h) = (native.cfa_width as usize, native.cfa_height as usize);
        if !matches!((cfa_w, cfa_h), (2, 2) | (6, 6)) {
            return Err(RawError::UnsupportedCfa);
        }
        let cfa = native.cfa[..cfa_w * cfa_h].to_vec();
        let black_cfa = native.black_cfa[..cfa_w * cfa_h].to_vec();
        if cfa.iter().any(|&c| c > 2) {
            return Err(RawError::UnsupportedCfa);
        }
        if black_cfa
            .iter()
            .zip(&cfa)
            .any(|(&site, &channel)| site > 3 || (if site == 3 { 1 } else { site }) != channel)
        {
            return Err(RawError::UnsupportedCfa);
        }
        let counts = [0_u8, 1, 2].map(|color| cfa.iter().filter(|&&v| v == color).count());
        if (cfa_w == 2 && counts != [1, 2, 1]) || (cfa_w == 6 && counts != [8, 20, 8]) {
            return Err(RawError::UnsupportedCfa);
        }
        let repeat_len = (native.black_repeat_width as usize)
            .checked_mul(native.black_repeat_height as usize)
            .ok_or(RawError::ResourceLimit("black pattern overflow"))?;
        if (native.black_repeat_width == 0) != (native.black_repeat_height == 0) {
            return Err(RawError::MissingCalibration(
                "incomplete black repeat dimensions",
            ));
        }
        if repeat_len > 4096
            || native.black_repeat_width > u8::MAX as u32
            || native.black_repeat_height > u8::MAX as u32
        {
            return Err(RawError::ResourceLimit("black pattern size"));
        }
        let black_repeat = native.black_repeat[..repeat_len].to_vec();
        if !native.white.is_finite() || native.white <= 0.0 {
            return Err(RawError::MissingCalibration("sensor white"));
        }
        if !native.black_base.is_finite()
            || native.black_base < 0.0
            || !native
                .black_channels
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0)
            || !black_repeat.iter().all(|v| v.is_finite() && *v >= 0.0)
        {
            return Err(RawError::MissingCalibration("black levels"));
        }
        let max_black = native.black_base
            + native
                .black_channels
                .iter()
                .copied()
                .fold(0.0_f32, f32::max)
            + black_repeat.iter().copied().fold(0.0_f32, f32::max);
        if !max_black.is_finite() || max_black >= native.white {
            return Err(RawError::MissingCalibration("black >= white"));
        }
        let mut as_shot = native.as_shot;
        if !as_shot.iter().all(|v| v.is_finite() && *v > 0.0) {
            return Err(RawError::MissingCalibration("as-shot WB"));
        }
        let green = as_shot[1];
        for gain in &mut as_shot {
            *gain /= green;
        }
        if !as_shot
            .iter()
            .all(|v| v.is_finite() && *v > 0.0 && *v <= 16.0)
        {
            return Err(RawError::MissingCalibration("normalized as-shot WB"));
        }
        if !native.rgb_cam.iter().all(|v| v.is_finite())
            || !native.cam_xyz.iter().all(|v| v.is_finite())
        {
            return Err(RawError::MissingCalibration("camera color matrix"));
        }
        let row_norms = (0..3)
            .map(|row| {
                native.rgb_cam[row * 4..row * 4 + 3]
                    .iter()
                    .map(|v| v.abs())
                    .sum::<f32>()
            })
            .collect::<Vec<_>>();
        if row_norms.iter().any(|v| !v.is_finite() || *v < 1e-8) {
            return Err(RawError::MissingCalibration("camera color matrix"));
        }
        let m = &native.rgb_cam;
        let det = m[0] * (m[5] * m[10] - m[6] * m[9]) - m[1] * (m[4] * m[10] - m[6] * m[8])
            + m[2] * (m[4] * m[9] - m[5] * m[8]);
        if !det.is_finite() || det.abs() < 1e-8 {
            return Err(RawError::MissingCalibration("singular camera color matrix"));
        }
        let rgb_cam = std::array::from_fn(|y| std::array::from_fn(|x| native.rgb_cam[y * 4 + x]));
        if rgb_cam.iter().any(|row| row[3] != 0.0) {
            return Err(RawError::MissingCalibration(
                "fourth camera color component unsupported",
            ));
        }
        let cam_xyz = std::array::from_fn(|y| std::array::from_fn(|x| native.cam_xyz[y * 3 + x]));
        let mut warnings = Vec::new();
        let dng_container = profile
            .dng
            .as_ref()
            .map(|settings| {
                format::dng_container(
                    bytes,
                    native,
                    &settings.container,
                    settings.decoder_active_bottom_trim,
                )
            })
            .transpose()?;
        let default_crop = match profile.crop {
            Crop::RafTags => {
                // The format's own crop tags are authoritative for this capability.
                let raf = raf_default_crop(bytes)
                    .ok_or(RawError::InvalidInput("missing RAF crop tags"))?;
                let parsed = checked_rect(raf, native.width, native.height)?;
                if inset.is_some_and(|inset| parsed != inset) {
                    warnings.push("LibRaw inset differs from RAF camera crop".to_string());
                }
                parsed
            }
            Crop::DngTags => {
                let container = dng_container
                    .as_ref()
                    .expect("validated DNG crop capability");
                if inset.is_some_and(|inset| container.default_crop != inset) {
                    warnings.push("LibRaw inset differs from DNG DefaultCrop tags".to_string());
                }
                container.default_crop
            }
            Crop::LibrawInset => inset.ok_or(RawError::InvalidInput("missing LibRaw inset"))?,
            Crop::ActiveArea => active,
        };
        let (cam_xyz, dng_correction) = if let Some(settings) = &profile.dng {
            let (matrix, calibration) = format::dng_color_calibration(bytes, native, settings)?;
            let correction = dng::DngCorrection::parse(
                opcodes,
                dng_container
                    .as_ref()
                    .expect("validated DNG crop capability")
                    .active_area,
                dng_container
                    .expect("validated DNG processing capability")
                    .raw_ifd,
                calibration,
                settings,
            )?;
            (matrix, Some(correction))
        } else {
            (cam_xyz, None)
        };
        validate_camera_response(&cam_xyz)?;
        let dng_corrections = dng_correction.as_ref().map(|v| v.metadata.clone());
        Ok((
            RawMetadata {
                make,
                model,
                mode,
                sensor_width: native.width,
                sensor_height: native.height,
                active_area: dng_container
                    .as_ref()
                    .map_or(active, |container| container.active_area),
                default_crop,
                cfa_width: cfa_w as u8,
                cfa_height: cfa_h as u8,
                cfa,
                black_cfa,
                black_base: native.black_base,
                black_channels: native.black_channels,
                black_repeat_width: native.black_repeat_width as u8,
                black_repeat_height: native.black_repeat_height as u8,
                black_repeat,
                sensor_white: native.white,
                as_shot_gains: as_shot,
                libraw_flip: native.flip as u8,
                exif_orientation: exif_orientation(native.flip)?,
                rgb_cam,
                cam_xyz,
                backend: PROVIDER.to_string(),
                libraw_inset: inset,
                format_identity: decoder,
                warnings,
                dng_corrections,
            },
            dng_correction,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_singular_camera_response_is_not_render_support() {
        assert!(matches!(
            validate_camera_response(&[[0.0; 3]; 4]),
            Err(RawError::MissingCalibration(_))
        ));
        assert!(validate_camera_response(&[[1.0; 3], [1.0; 3], [1.0; 3], [0.0; 3]]).is_err());
        let mut matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]];
        assert!(validate_camera_response(&matrix).is_ok());
        matrix[0][0] = f32::NAN;
        assert!(validate_camera_response(&matrix).is_err());
    }

    #[test]
    fn libraw_inset_absence_is_distinct_from_malformed_rectangles() {
        let mut native = RawSource::blank_native();
        native.inset_width = 0;
        native.inset_height = 0;
        assert_eq!(libraw_inset(&native, 100, 80).unwrap(), None);

        native.inset_x = 65_535;
        native.inset_y = 65_535;
        native.inset_width = 6000;
        native.inset_height = 4000;
        assert_eq!(libraw_inset(&native, 100, 80).unwrap(), None);

        native.inset_x = 0;
        native.inset_y = 0;
        native.inset_width = 99;
        native.inset_height = 0;
        assert!(matches!(
            libraw_inset(&native, 100, 80),
            Err(RawError::InvalidInput("crop outside sensor"))
        ));
    }

    #[test]
    fn active_area_crop_is_available_when_libraw_inset_is_absent() {
        let mut native = RawSource::blank_native();
        native.width = 100;
        native.height = 80;
        native.active_x = 2;
        native.active_y = 3;
        native.active_width = 96;
        native.active_height = 74;
        native.inset_width = 0;
        native.inset_height = 0;
        let inset = libraw_inset(&native, native.width, native.height).unwrap();
        assert_eq!(inset, None);
        let active = checked_rect(
            RawRect {
                x: native.active_x,
                y: native.active_y,
                width: native.active_width,
                height: native.active_height,
            },
            native.width,
            native.height,
        )
        .unwrap();
        assert_eq!(
            active,
            RawRect {
                x: 2,
                y: 3,
                width: 96,
                height: 74
            }
        );
    }

    #[test]
    fn native_calibration_uses_both_bayer_green_sites() {
        let mut native = RawSource::blank_native();
        native.width = 16;
        native.height = 16;
        native.cfa_width = 2;
        native.cfa_height = 2;
        native.cfa = [
            0, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0,
        ];
        native.black_cfa = [
            0, 1, 3, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0,
        ];
        native.black_base = 100.0;
        native.black_channels = [10.0, 20.0, 30.0, 40.0];
        native.white = 1000.0;
        let samples: Vec<u16> = (0..256)
            .map(|i| match ((i / 16) % 2) * 2 + (i % 16) % 2 {
                0 => 110,
                1 => 120,
                2 => 140,
                _ => 130,
            })
            .collect();
        let mut red = vec![f32::NAN; samples.len()];
        let mut green = vec![f32::NAN; samples.len()];
        let mut blue = vec![f32::NAN; samples.len()];
        let gains = [1.0_f32; 3];
        let mut error = [0 as c_char; 128];
        let cancel = AtomicBool::new(false);
        let code = unsafe {
            lw_raw_develop(
                samples.as_ptr(),
                samples.len(),
                &native,
                std::ptr::null(),
                0,
                gains.as_ptr(),
                red.as_mut_ptr(),
                green.as_mut_ptr(),
                blue.as_mut_ptr(),
                0,
                None,
                std::ptr::null_mut(),
                cancelled,
                (&cancel as *const AtomicBool).cast_mut().cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_eq!(code, 0);
        for plane in [&red[..], &green[..], &blue[..]] {
            assert!(
                plane
                    .chunks_exact(16)
                    .skip(3)
                    .take(10)
                    .flat_map(|row| row[3..13].iter())
                    .all(|value| value.abs() < 1e-6),
                "{plane:?}"
            );
        }

        // A declared repair substitutes the bad site only in this develop
        // pass. The retained decoded mosaic remains exactly unchanged.
        let expected = [red.clone(), green.clone(), blue.clone()];
        let mut damaged = samples.clone();
        let repair = MosaicCorrection {
            index: 8 * 16 + 8,
            value: damaged[8 * 16 + 8],
        };
        damaged[repair.index as usize] = 0;
        let damaged_before = damaged.clone();
        let code = unsafe {
            lw_raw_develop(
                damaged.as_ptr(),
                damaged.len(),
                &native,
                &repair,
                1,
                gains.as_ptr(),
                red.as_mut_ptr(),
                green.as_mut_ptr(),
                blue.as_mut_ptr(),
                0,
                None,
                std::ptr::null_mut(),
                cancelled,
                (&cancel as *const AtomicBool).cast_mut().cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_eq!(code, 0);
        assert_eq!([red.clone(), green.clone(), blue.clone()], expected);
        assert_eq!(damaged, damaged_before);

        // A merged green map would incorrectly use site 1's black for site 3.
        native.black_cfa[2] = 1;
        let code = unsafe {
            lw_raw_develop(
                samples.as_ptr(),
                samples.len(),
                &native,
                std::ptr::null(),
                0,
                gains.as_ptr(),
                red.as_mut_ptr(),
                green.as_mut_ptr(),
                blue.as_mut_ptr(),
                0,
                None,
                std::ptr::null_mut(),
                cancelled,
                (&cancel as *const AtomicBool).cast_mut().cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_eq!(code, 0);
        assert!(green[3 * 16 + 3] > 1e-4);
    }

    #[test]
    #[ignore = "requires local DJI original and an explicit temporary output path"]
    fn dump_dji_sparse_uncorrected_reference() {
        use std::fmt::Write;
        let source = std::env::var("LIGHTWELL_DNG_SOURCE").expect("DNG source path");
        let output = std::env::var("LIGHTWELL_DNG_REFERENCE_DUMP").expect("temporary output path");
        let bytes = Arc::from(std::fs::read(source).expect("read original"));
        let cancel = AtomicBool::new(false);
        let raw = RawSource::decode(bytes, &cancel).expect("decode DNG");
        let before = raw
            .develop_uncorrected(raw.metadata.as_shot_gains, &cancel)
            .expect("develop uncorrected camera planes");
        let after = raw
            .develop(raw.metadata.as_shot_gains, &cancel)
            .expect("develop corrected camera planes");
        let n = before.plane_len();
        let width = before.width as usize;
        let active = raw.metadata.active_area;
        let mut csv = String::from("kind,out_x,out_y,channel,sensor_x,sensor_y,value\n");
        for (x, y) in [
            (100, 4),
            (320, 240),
            (1400, 850),
            (2840, 1824),
            (4670, 2900),
            (5563, 3643),
        ] {
            for channel in 0..3 {
                let idx = channel * n + y as usize * width + x as usize;
                writeln!(csv, "result,{x},{y},{channel},,,{}", after.data[idx]).unwrap();
                let (sx, sy) = raw.corrected_sensor_sample_location(x, y, channel).unwrap();
                writeln!(csv, "mapped,{x},{y},{channel},{sx},{sy},").unwrap();
                for yy in -3..=4 {
                    for xx in -3..=4 {
                        let px = ((sx.floor() as i64 + xx)
                            .clamp(active.x as i64, (active.x + active.width - 1) as i64))
                            as usize;
                        let py = ((sy.floor() as i64 + yy)
                            .clamp(active.y as i64, (active.y + active.height - 1) as i64))
                            as usize;
                        let v = before.data[channel * n + py * width + px];
                        writeln!(csv, "input,{x},{y},{channel},{px},{py},{v}").unwrap();
                    }
                }
            }
        }
        std::fs::write(output, csv).expect("write temporary sparse reference");
    }
    #[test]
    #[ignore = "requires explicit local authentic Fuji RAW fixture"]
    fn markesteijn_parallel_complete_float_oracle() {
        use sha2::{Digest, Sha256};
        fn without_executor(raw: &RawSource, gains: [f32; 3], cancel: &AtomicBool) -> Vec<f32> {
            let n = raw.mosaic.len();
            let mut data = vec![0.0_f32; n * 3];
            let (red, rest) = data.split_at_mut(n);
            let (green, blue) = rest.split_at_mut(n);
            let mut error = [0 as c_char; 256];
            // SAFETY: buffers and callback token remain live for the native
            // synchronous call; this exercises its default serial executor.
            let code = unsafe {
                lw_raw_develop(
                    raw.mosaic.as_ptr(),
                    n,
                    &*raw.native,
                    raw.mosaic_corrections.as_ptr(),
                    raw.mosaic_corrections.len(),
                    gains.as_ptr(),
                    red.as_mut_ptr(),
                    green.as_mut_ptr(),
                    blue.as_mut_ptr(),
                    0,
                    None,
                    std::ptr::null_mut(),
                    cancelled,
                    (cancel as *const AtomicBool).cast_mut().cast(),
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert_eq!(code, 0, "{}", c_text(&error));
            data
        }
        let owner = std::env::var("LIGHTWELL_RAW_OWNER_DIR").expect("RAW fixture directory");
        let path = format!("{owner}/fujifilm_x100vi.RAF");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "187e3403bdb93617a906e059209e91037e56efbaf5b6824700e96e0d4abd7246"
        );
        let cancel = AtomicBool::new(false);
        let raw = RawSource::decode(Arc::from(bytes), &cancel).unwrap();
        let gains = raw.metadata.as_shot_gains;
        let serial = raw
            .develop_uncorrected_with_workers(gains, &cancel, 1)
            .unwrap();
        let default_serial = without_executor(&raw, gains, &cancel);
        assert!(
            default_serial
                .iter()
                .zip(&serial.data)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        drop(default_serial);
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let mut hash = Sha256::new();
            for value in &serial.data {
                hash.update(value.to_bits().to_le_bytes());
            }
            // Captured from the pre-change binary on the owner's M4 Mac.
            // Other architectures still run the complete serial/pooled
            // comparison below without claiming cross-platform bit parity.
            assert_eq!(
                format!("{:x}", hash.finalize()),
                "4174b34b43a8d4b684668a4cb9eebfc8d71ed88d5dc1e8057412f4bf5931f209"
            );
        }
        for workers in [2, 4, 0, 4] {
            let other = raw
                .develop_uncorrected_with_workers(gains, &cancel, workers)
                .unwrap();
            assert_eq!(other.data.len(), serial.data.len());
            if let Some(index) = other
                .data
                .iter()
                .zip(&serial.data)
                .position(|(a, b)| a.to_bits() != b.to_bits())
            {
                panic!(
                    "complete Fuji float planes differ at worker count {workers}, index {index}, serial {:?}, parallel {:?}",
                    serial.data[index].to_bits(),
                    other.data[index].to_bits()
                );
            }
        }
        let adjusted = [gains[0] * 1.15, 1.0, gains[2] * 0.85];
        let custom_serial = raw
            .develop_uncorrected_with_workers(adjusted, &cancel, 1)
            .unwrap();
        for workers in [2, 4, 0] {
            let other = raw
                .develop_uncorrected_with_workers(adjusted, &cancel, workers)
                .unwrap();
            assert!(
                other
                    .data
                    .iter()
                    .zip(&custom_serial.data)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "complete changed-WB Fuji planes differ at worker count {workers}"
            );
        }
        drop(custom_serial);
        drop(serial);

        // Odd final tiles exercise the retained interior and border join.
        let width = 239_usize;
        let height = 347_usize;
        let source_width = raw.native.width as usize;
        let mut odd = raw.clone();
        odd.native.width = width as u32;
        odd.native.height = height as u32;
        odd.metadata.sensor_width = width as u32;
        odd.metadata.sensor_height = height as u32;
        odd.mosaic = Arc::new(
            raw.mosaic
                .chunks_exact(source_width)
                .take(height)
                .flat_map(|row| row[..width].iter().copied())
                .collect(),
        );
        odd.mosaic_corrections = Arc::new(Vec::new());
        odd.dng_correction = None;
        let reference = odd
            .develop_uncorrected_with_workers(adjusted, &cancel, 1)
            .unwrap();
        let odd_default_serial = without_executor(&odd, adjusted, &cancel);
        assert!(
            odd_default_serial
                .iter()
                .zip(&reference.data)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        for workers in [2, 4, 0] {
            let other = odd
                .develop_uncorrected_with_workers(adjusted, &cancel, workers)
                .unwrap();
            assert!(
                other
                    .data
                    .iter()
                    .zip(&reference.data)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
        std::thread::scope(|scope| {
            let first = scope.spawn(|| odd.develop_uncorrected_with_workers(adjusted, &cancel, 0));
            let second = scope.spawn(|| odd.develop_uncorrected_with_workers(adjusted, &cancel, 0));
            for result in [first.join().unwrap(), second.join().unwrap()] {
                let other = result.unwrap();
                assert!(
                    other
                        .data
                        .iter()
                        .zip(&reference.data)
                        .all(|(a, b)| a.to_bits() == b.to_bits())
                );
            }
        });
        for (width, height) in [
            (119, 413),
            (413, 119),
            (120, 413),
            (121, 221),
            (219, 219),
            (219, 121),
            (317, 315),
            (317, 120),
            (1003, 413),
            (1004, 511),
            (1005, 413),
        ] {
            let mut edge = raw.clone();
            edge.native.width = width as u32;
            edge.native.height = height as u32;
            edge.metadata.sensor_width = width as u32;
            edge.metadata.sensor_height = height as u32;
            edge.mosaic = Arc::new(
                raw.mosaic
                    .chunks_exact(source_width)
                    .take(height)
                    .flat_map(|row| row[..width].iter().copied())
                    .collect(),
            );
            edge.mosaic_corrections = Arc::new(Vec::new());
            edge.dng_correction = None;
            if width < 120 || height < 120 {
                assert!(matches!(
                    edge.develop_uncorrected_with_workers(adjusted, &cancel, 1),
                    Err(RawError::ResourceLimit(_))
                ));
                assert!(matches!(
                    edge.develop_uncorrected_with_workers(adjusted, &cancel, 0),
                    Err(RawError::ResourceLimit(_))
                ));
                continue;
            }
            let serial = edge
                .develop_uncorrected_with_workers(adjusted, &cancel, 1)
                .unwrap();
            let default_serial = without_executor(&edge, adjusted, &cancel);
            assert!(
                default_serial
                    .iter()
                    .zip(&serial.data)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "default serial differs at edge geometry {width}x{height}"
            );
            let pooled = edge
                .develop_uncorrected_with_workers(adjusted, &cancel, 0)
                .unwrap();
            assert!(
                serial
                    .data
                    .iter()
                    .zip(&pooled.data)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "edge geometry {width}x{height}"
            );
        }
        let token = AtomicBool::new(true);
        assert!(matches!(
            odd.develop_uncorrected_with_workers(adjusted, &token, 4),
            Err(RawError::Cancelled)
        ));

        struct CancelAfter {
            calls: std::sync::atomic::AtomicUsize,
            first_cancel_call: usize,
        }
        extern "C" fn cancel_after(context: *mut c_void) -> c_int {
            // SAFETY: the native call joins all tile callbacks before the
            // stack-backed counter leaves this test.
            let state = unsafe { &*context.cast::<CancelAfter>() };
            c_int::from(state.calls.fetch_add(1, Ordering::Relaxed) >= state.first_cancel_call)
        }
        let run_private_fault = |fault: u32, first_cancel_call: usize, worker_limit: usize| {
            let n = odd.mosaic.len();
            let mut data = vec![f32::NAN; n * 3];
            let (red, rest) = data.split_at_mut(n);
            let (green, blue) = rest.split_at_mut(n);
            let state = CancelAfter {
                calls: std::sync::atomic::AtomicUsize::new(0),
                first_cancel_call,
            };
            let mut executor_context = native_tiles::ExecutorContext {
                cancel: &cancel,
                worker_limit,
            };
            let mut error = [0 as c_char; 256];
            // SAFETY: all input/output/callback state is live and disjoint;
            // the synchronous native and Rayon entries join before return.
            let code = unsafe {
                lw_raw_develop(
                    odd.mosaic.as_ptr(),
                    n,
                    &*odd.native,
                    std::ptr::null(),
                    0,
                    adjusted.as_ptr(),
                    red.as_mut_ptr(),
                    green.as_mut_ptr(),
                    blue.as_mut_ptr(),
                    fault,
                    Some(native_tiles::execute),
                    (&mut executor_context as *mut native_tiles::ExecutorContext<'_>).cast(),
                    cancel_after,
                    (&state as *const CancelAfter).cast_mut().cast(),
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            (
                code,
                native_error(code, &error),
                state.calls.load(Ordering::Relaxed),
            )
        };
        // The adapter checks once before normalization and at rows 0, 128,
        // and 256. Native tile checks then allow work before cancellation.
        let (code, error, calls) = run_private_fault(0, 6, 1);
        assert_eq!(code, 2);
        assert!(matches!(error, RawError::Cancelled));
        assert!(calls >= 7);
        let (code, error, _) = run_private_fault(1, usize::MAX, 4);
        assert_eq!(code, 6);
        assert!(matches!(error, RawError::ResourceLimit(_)));
        let (code, error, _) = run_private_fault(2, usize::MAX, 4);
        assert_eq!(code, 3);
        assert!(matches!(error, RawError::Native(_)));
        // A successful call after both failures proves all workers joined
        // and returned their process-wide scratch permits.
        let recovered = odd
            .develop_uncorrected_with_workers(adjusted, &cancel, 4)
            .unwrap();
        assert!(
            recovered
                .data
                .iter()
                .zip(&reference.data)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
            "187e3403bdb93617a906e059209e91037e56efbaf5b6824700e96e0d4abd7246"
        );
    }

    #[test]
    fn early_cancellation_and_empty_source() {
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            RawSource::decode(Arc::from(&b"anything"[..]), &cancelled),
            Err(RawError::Cancelled)
        ));
        let active = AtomicBool::new(false);
        assert!(matches!(
            RawSource::decode(Arc::from(&b""[..]), &active),
            Err(RawError::InvalidInput(_))
        ));
    }
    #[test]
    fn dng_opcode_allowance_requires_processing_capability() {
        let required = format::DngOpcode {
            ifd: 0,
            list: 51022,
            id: 9,
            version: 0x0103_0000,
            flags: 0,
            data: vec![],
        };
        assert!(matches!(
            reject_unhandled_required_opcodes(false, std::slice::from_ref(&required)),
            Err(RawError::UnsupportedRequiredOpcodes(ids)) if ids == vec![9]
        ));
        assert!(reject_unhandled_required_opcodes(true, &[required]).is_ok());
    }
    #[test]
    fn geometry_and_orientation_are_bounded() {
        let mut n = RawSource::blank_native();
        n.width = 16_385;
        n.height = 1;
        assert!(matches!(
            RawSource::checked_len(&n),
            Err(RawError::ResourceLimit(_))
        ));
        n.width = 16_384;
        n.height = 16_384;
        assert!(matches!(
            RawSource::checked_len(&n),
            Err(RawError::ResourceLimit(_))
        ));
        n.width = 16_000;
        n.height = 8_000;
        let admitted = RawSource::checked_len(&n).unwrap();
        assert_eq!(admitted, MAX_PIXELS);
        assert!(admitted * 3 * std::mem::size_of::<f32>() <= MAX_RGB_BYTES);
        n.height += 1;
        assert!(matches!(
            RawSource::checked_len(&n),
            Err(RawError::ResourceLimit(_))
        ));
        assert_eq!(exif_orientation(5), Ok(8));
        assert!(exif_orientation(8).is_err());
        assert!(
            checked_rect(
                RawRect {
                    x: 99,
                    y: 0,
                    width: 2,
                    height: 1
                },
                100,
                1
            )
            .is_err()
        );
    }
}
