//! Decoding an original into pixels: the JPEG path (bounded header validation, upright decode,
//! RGBA written straight into the frame the render returns) and the prepared original, byte-exact
//! JPEG or an immutable RAW mosaic with one WB development.
use crate::{Error, ErrorKind, LinearImage, Raster, colour::mat3::matvec_f32};
use image::{ImageDecoder, ImageReader, Limits};
use lightwell_raw::{RawError, RawMetadata, RawSource};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

/// The encoded-byte limit for a JPEG original; a RAW original's own limit is
/// `lightwell_raw::MAX_SOURCE_BYTES`.
pub(crate) const MAX_JPEG_BYTES: usize = 128 * 1024 * 1024;

/// The complete upright source, decoded once for non-destructive recipe evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    pub fingerprint: String,
    pub orientation: u8,
}

fn decode_error(detail: &str) -> Error {
    Error::new(ErrorKind::Decode, detail)
}

// Walk JPEG header segments without decoding or allocating from declared dimensions.
fn header(bytes: &[u8]) -> Result<(u32, u32, u8), Error> {
    if !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        return Err(decode_error("missing JPEG SOI/EOI"));
    }
    let mut i = 2;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xff {
            return Err(decode_error("JPEG marker"));
        }
        while i < bytes.len() && bytes[i] == 0xff {
            i += 1;
        }
        let marker = *bytes.get(i).ok_or_else(|| decode_error("marker"))?;
        i += 1;
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        let size = bytes
            .get(i..i + 2)
            .ok_or_else(|| decode_error("segment length"))?;
        let size = u16::from_be_bytes([size[0], size[1]]) as usize;
        if size < 2 || i + size > bytes.len() {
            return Err(decode_error("segment bounds"));
        }
        if [0xc0, 0xc1, 0xc2].contains(&marker) {
            if size < 8 || bytes[i + 2] != 8 {
                return Err(Error::new(ErrorKind::UnsupportedColor, "JPEG precision"));
            }
            let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            return Ok((w, h, bytes[i + 7]));
        }
        i += size;
    }
    Err(Error::new(ErrorKind::UnsupportedInput, "JPEG frame type"))
}

/// Hash and decode one bounded snapshot read from an already opened handle. The magic bytes pick
/// the limit: a JPEG original is bounded by [`MAX_JPEG_BYTES`], anything else by RAW's own bound.
pub(crate) fn read_bounded_file(file: &mut File) -> Result<Vec<u8>, Error> {
    let file_error = |e: std::io::Error| Error::new(ErrorKind::FileAccess, e.kind().to_string());
    if !file.metadata().map_err(file_error)?.is_file() {
        return Err(Error::new(
            ErrorKind::UnsupportedInput,
            "expected a regular file",
        ));
    }
    let mut magic = [0_u8; 2];
    let _ = file.read(&mut magic).map_err(file_error)?;
    file.seek(SeekFrom::Start(0)).map_err(file_error)?;
    let limit = if magic == [0xff, 0xd8] {
        MAX_JPEG_BYTES
    } else {
        lightwell_raw::MAX_SOURCE_BYTES
    };
    if file.metadata().map_err(file_error)?.len() > limit as u64 {
        return Err(Error::new(ErrorKind::ResourceLimit, "encoded bytes"));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(file_error)?;
    if bytes.len() > limit {
        return Err(Error::new(ErrorKind::ResourceLimit, "encoded bytes"));
    }
    Ok(bytes)
}

struct Decoded {
    upright: image::DynamicImage,
    orientation: u8,
}

/// Validate the supported JPEG subset, decode within fixed limits and orient once to upright pixels.
fn decode_upright(bytes: Vec<u8>) -> Result<Decoded, Error> {
    let (w, h, components) = header(&bytes)?;
    if w == 0 || h == 0 || w > 16384 || h > 16384 || u64::from(w) * u64::from(h) > 64_000_000 {
        return Err(Error::new(ErrorKind::ResourceLimit, "dimensions"));
    }
    if ![1, 3].contains(&components) {
        return Err(Error::new(
            ErrorKind::UnsupportedColor,
            "only RGB/greyscale",
        ));
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| decode_error("decoder header"))?;
    if let Some(profile) = decoder
        .icc_profile()
        .map_err(|_| Error::new(ErrorKind::UnsupportedProfile, "unreadable ICC"))?
    {
        crate::profile::check(&profile, components)?;
    }
    let orientation = decoder
        .orientation()
        .map_err(|_| decode_error("orientation"))?;
    let mut upright =
        image::DynamicImage::from_decoder(decoder).map_err(|_| decode_error("decode"))?;
    upright.apply_orientation(orientation);
    Ok(Decoded {
        upright,
        orientation: orientation.to_exif(),
    })
}

/// Write `upright`'s pixels into `out` as RGBA, opaque, without an intermediate allocation. `out`
/// must be exactly `width * height * 4` bytes, the shape [`decode_upright`]'s caller allocates
/// through [`crate::render::zeroed_frame`].
fn write_rgba(upright: &image::DynamicImage, out: &mut [u8]) -> Result<(), Error> {
    match upright {
        image::DynamicImage::ImageRgb8(buf) => {
            for (dst, src) in out.chunks_exact_mut(4).zip(buf.as_raw().chunks_exact(3)) {
                dst[0] = src[0];
                dst[1] = src[1];
                dst[2] = src[2];
                dst[3] = 255;
            }
            Ok(())
        }
        image::DynamicImage::ImageLuma8(buf) => {
            for (dst, src) in out.chunks_exact_mut(4).zip(buf.as_raw().iter()) {
                dst[0] = *src;
                dst[1] = *src;
                dst[2] = *src;
                dst[3] = 255;
            }
            Ok(())
        }
        _ => Err(decode_error("unexpected decoded color type")),
    }
}

/// Decode the complete upright source once for non-destructive recipe evaluation.
pub fn open_source(path: &Path) -> Result<SourceImage, Error> {
    let mut file =
        File::open(path).map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
    open_source_file(&mut file)
}

pub(crate) fn open_source_file(file: &mut File) -> Result<SourceImage, Error> {
    open_source_bytes(read_bounded_file(file)?)
}

/// Decode straight into the shared frame a [`Raster`] would hold, so a JPEG's decoded pixels are
/// written once: no intermediate RGBA buffer that this then copies into an `Arc<[u8]>`.
pub(crate) fn open_source_bytes(bytes: Vec<u8>) -> Result<SourceImage, Error> {
    let fingerprint = format!("{:x}", Sha256::digest(&bytes));
    let decoded = decode_upright(bytes)?;
    let width = decoded.upright.width();
    let height = decoded.upright.height();
    let mut frame = crate::render::zeroed_frame(Raster::expected_len(width, height)?);
    write_rgba(&decoded.upright, crate::render::frame_mut(&mut frame))?;
    Ok(SourceImage {
        width,
        height,
        rgba: frame,
        fingerprint,
        orientation: decoded.orientation,
    })
}

use rayon::prelude::*;

const PARALLEL_CAMERA_PIXELS: usize = 1_000_000;
const CAMERA_CHUNK_PIXELS: usize = 65_536;

/// Convert the existing planar allocation in disjoint slices. A camera pixel is read into three
/// locals before any output channel is written, preserving the scalar operation order. Each
/// successfully finished chunk has proved all three resulting values finite, which permits the
/// private linear-image adoption path to skip a second full-frame scan.
///
/// This is the one finiteness check between decode and adoption, guarding the finite-planes
/// contract of [`LinearImage`]. The RAW crate's development and DNG corrections check nothing: an
/// overflow or non-finite value they produce reaches this loop, and a non-finite camera value makes
/// every converted channel of its pixel non-finite whatever the matrix (`0 × ∞` is NaN).
fn convert_camera_planes(
    planes: &mut [f32],
    n: usize,
    matrix: &[[f32; 4]; 3],
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let (red, rest) = planes.split_at_mut(n);
    let (green, blue) = rest.split_at_mut(n);
    let matrix = matrix.map(|row| [row[0], row[1], row[2]]);
    let convert = |red: &mut [f32], green: &mut [f32], blue: &mut [f32]| {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::new(ErrorKind::Conflict, "RAW development cancelled"));
        }
        for i in 0..red.len() {
            let [r, g, b] = matvec_f32(&matrix, [red[i], green[i], blue[i]]);
            if !r.is_finite() || !g.is_finite() || !b.is_finite() {
                return Err(Error::new(
                    ErrorKind::UnsupportedColor,
                    "RAW color conversion produced a non-finite value",
                ));
            }
            red[i] = r;
            green[i] = g;
            blue[i] = b;
        }
        Ok(())
    };
    if n >= PARALLEL_CAMERA_PIXELS {
        red.par_chunks_mut(CAMERA_CHUNK_PIXELS)
            .zip(green.par_chunks_mut(CAMERA_CHUNK_PIXELS))
            .zip(blue.par_chunks_mut(CAMERA_CHUNK_PIXELS))
            .try_for_each(|((red, green), blue)| convert(red, green, blue))?;
    } else {
        for ((red, green), blue) in red
            .chunks_mut(CAMERA_CHUNK_PIXELS)
            .zip(green.chunks_mut(CAMERA_CHUNK_PIXELS))
            .zip(blue.chunks_mut(CAMERA_CHUNK_PIXELS))
        {
            convert(red, green, blue)?;
        }
    }
    // A cancellation in the final short chunk must never publish these partially converted
    // planes. Rayon has joined every chunk before this check or before returning an error.
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::new(ErrorKind::Conflict, "RAW development cancelled"));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) enum PreparedSource {
    Jpeg(SourceImage),
    Raw(RawPrepared),
}

#[derive(Clone, Debug)]
pub(crate) struct RawPrepared {
    pub(crate) sensor: Arc<RawSource>,
    pub(crate) linear: Option<LinearImage>,
    pub(crate) gains: [f32; 3],
}

/// A known recipe's source-only target, resolved without reading pixels on the catalog owner.
/// The freshly unpacked interpretation must match before its gains can develop that mosaic.
#[derive(Clone, Debug)]
pub(crate) struct RawPreparation {
    pub(crate) metadata: RawMetadata,
    pub(crate) gains: [f32; 3],
}

impl RawPreparation {
    pub(crate) fn validate(&self, metadata: &RawMetadata) -> Result<(), Error> {
        // Both sides are typed first, so catalog JSON's shortest f32 decimals compare at the
        // native precision, exactly as they do when the owner adopts the completed source.
        let value = |metadata: &RawMetadata| {
            serde_json::to_value(metadata)
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
        };
        if value(&self.metadata)? != value(metadata)? {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "original source interpretation changed",
            ));
        }
        Ok(())
    }
}

impl PreparedSource {
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Jpeg(image) => (image.width, image.height),
            Self::Raw(raw) => {
                let metadata = raw.sensor.metadata();
                let crop = metadata.default_crop;
                if (5..=8).contains(&metadata.exif_orientation) {
                    (crop.height, crop.width)
                } else {
                    (crop.width, crop.height)
                }
            }
        }
    }
    pub(crate) fn metadata(&self) -> Option<&RawMetadata> {
        match self {
            Self::Jpeg(_) => None,
            Self::Raw(raw) => Some(raw.sensor.metadata()),
        }
    }
}

pub(crate) fn raw_error(error: RawError) -> Error {
    let kind = match error {
        RawError::ResourceLimit(_) => ErrorKind::ResourceLimit,
        RawError::Cancelled => ErrorKind::Conflict,
        RawError::UnsupportedMode(_)
        | RawError::UnsupportedRequiredOpcodes(_)
        | RawError::UnsupportedCfa => ErrorKind::UnsupportedInput,
        RawError::MissingCalibration(_) => ErrorKind::UnsupportedColor,
        RawError::InvalidInput(_) | RawError::Native(_) => ErrorKind::Decode,
        RawError::NeutralPatch(_) => ErrorKind::Validation,
    };
    Error::new(kind, error.to_string())
}

impl RawPrepared {
    /// Decode a RAW original and develop it at `gains`, or at the camera's as-shot gains when none
    /// are named, so a preparation for a stack with its own white balance develops it once.
    pub(crate) fn decode(
        bytes: Vec<u8>,
        fingerprint: String,
        target: Option<&RawPreparation>,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        // The file's bytes go to the decoder as read: no copy into another buffer.
        let sensor = Arc::new(RawSource::decode(bytes, cancel).map_err(raw_error)?);
        let gains = match target {
            Some(target) => {
                target.validate(sensor.metadata())?;
                target.gains
            }
            None => sensor.metadata().as_shot_gains,
        };
        Self::develop(sensor, fingerprint, gains, cancel)
    }

    pub(crate) fn develop(
        sensor: Arc<RawSource>,
        fingerprint: String,
        gains: [f32; 3],
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let mut rgb = sensor.develop(gains, cancel).map_err(raw_error)?;
        let n = rgb.plane_len();
        convert_camera_planes(&mut rgb.data, n, &sensor.metadata().rgb_cam, cancel)?;
        let metadata = sensor.metadata();
        let crop = metadata.default_crop;
        let linear =
            LinearImage::from_validated_planes(rgb.width, rgb.height, rgb.data, fingerprint)?
                .with_view(
                    [crop.x, crop.y, crop.width, crop.height],
                    metadata.exif_orientation,
                )?;
        Ok(Self {
            sensor,
            linear: Some(linear),
            gains,
        })
    }
}

/// The sensor white-balance gains that neutralise the patch around an upright content point.
/// The RAW crate reads the retained mosaic ([`RawSource::neutral_gains_at`]); neither float
/// development nor full-frame work runs on the owner.
pub(crate) fn neutral_at(raw: &RawPrepared, x: u32, y: u32) -> Result<[f32; 3], Error> {
    raw.sensor.neutral_gains_at(x, y).map_err(raw_error)
}

/// The source worker's memory gate: before it allocates another RAW development, the worker waits
/// until every development it produced earlier has been released by the caches and previews that
/// held it. Each adopted development carries a [`PlaneLease`], shared by every view and clone of
/// its planes; the last one dropping wakes the worker. A cancelled job wakes it through
/// [`Self::wake`]. Nothing polls.
#[derive(Default)]
pub(crate) struct PlaneGate {
    live: Mutex<usize>,
    released: Condvar,
}

impl PlaneGate {
    fn live(&self) -> MutexGuard<'_, usize> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A hold on the gate for one development's planes, released when it drops.
    pub(crate) fn lease(self: &Arc<Self>) -> PlaneLease {
        *self.live() += 1;
        PlaneLease(Arc::clone(self))
    }

    /// Block until no leased planes remain, or until `stop` is set and [`Self::wake`] called.
    /// Returns whether it stopped.
    pub(crate) fn wait_released(&self, stop: &AtomicBool) -> bool {
        let mut live = self.live();
        while *live > 0 && !stop.load(Ordering::Relaxed) {
            live = self
                .released
                .wait(live)
                .unwrap_or_else(PoisonError::into_inner);
        }
        stop.load(Ordering::Relaxed)
    }

    /// Wake a waiting worker to look at its job's stop flag again, after setting it. Taking the
    /// lock first means a worker that has just read the flag is already waiting when this notifies.
    pub(crate) fn wake(&self) {
        let _live = self.live();
        self.released.notify_all();
    }
}

/// One development's hold on the [`PlaneGate`].
pub(crate) struct PlaneLease(Arc<PlaneGate>);

impl Drop for PlaneLease {
    fn drop(&mut self) {
        let mut live = self.0.live();
        *live -= 1;
        if *live == 0 {
            self.0.released.notify_all();
        }
    }
}

/// Where a [`LinearImage`] keeps its development's [`PlaneLease`], shared by every view and clone.
/// Bookkeeping, not content: it never makes two images unequal.
#[derive(Clone, Default)]
pub(crate) struct PlanesHeld(Option<Arc<PlaneLease>>);

impl PlanesHeld {
    pub(crate) fn new(lease: PlaneLease) -> Self {
        Self(Some(Arc::new(lease)))
    }
}

impl PartialEq for PlanesHeld {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for PlanesHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "held" } else { "unheld" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LinearSettings, ModuleRegistry, PreviewSource, ProxyBounds, Recipe, SnapshotId,
        WhiteBalanceApproximation, gains_from_temperature_tint, render::testing::render_linear,
    };
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    /// The gate opens when the last view of a held development drops, on whichever thread drops
    /// it, and a stop wakes it while planes are still held. Waits are bounded by a watchdog so a
    /// lost wake-up fails the test rather than hanging it.
    #[test]
    fn plane_gate_opens_on_the_last_release_and_on_a_stop() {
        let gate = Arc::new(PlaneGate::default());
        let never = AtomicBool::new(false);
        assert!(!gate.wait_released(&never), "an idle gate is open");

        let mut image = LinearImage::new(2, 1, vec![0.0; 6]).unwrap();
        image.hold(gate.lease());
        let view = image.with_view([1, 0, 1, 1], 6).unwrap();
        let clone = image.clone();
        assert_eq!(clone, image, "a hold is not image content");
        drop(image);
        drop(clone);
        let (done, finished) = std::sync::mpsc::channel();
        let waiter = {
            let gate = gate.clone();
            std::thread::spawn(move || {
                let stopped = gate.wait_released(&AtomicBool::new(false));
                done.send(stopped).unwrap();
            })
        };
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "the gate waits while a view is alive"
        );
        std::thread::spawn(move || drop(view)).join().unwrap();
        let stopped = finished
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the last release wakes the worker");
        assert!(!stopped);
        waiter.join().unwrap();

        let mut held = LinearImage::new(1, 1, vec![0.0; 3]).unwrap();
        held.hold(gate.lease());
        let stop = Arc::new(AtomicBool::new(false));
        let (done, finished) = std::sync::mpsc::channel();
        let waiter = {
            let (gate, stop) = (gate.clone(), stop.clone());
            std::thread::spawn(move || done.send(gate.wait_released(&stop)).unwrap())
        };
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        stop.store(true, Ordering::Relaxed);
        gate.wake();
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("a stop wakes the worker")
        );
        waiter.join().unwrap();
        drop(held);
        assert!(!gate.wait_released(&never));
    }

    fn scalar_camera_reference(mut planes: Vec<f32>, n: usize, matrix: &[[f32; 4]; 3]) -> Vec<f32> {
        for i in 0..n {
            let camera = [planes[i], planes[n + i], planes[2 * n + i]];
            for (channel, row) in matrix.iter().enumerate() {
                planes[channel * n + i] =
                    row[0] * camera[0] + row[1] * camera[1] + row[2] * camera[2];
            }
        }
        planes
    }

    #[test]
    fn camera_chunks_match_independent_scalar_bits_across_awkward_edges() {
        let matrix = [
            [1.341, -0.123, 0.004, f32::MAX],
            [-0.061, 1.161, -0.100, f32::MAX],
            [0.002, -0.049, 1.047, f32::MAX],
        ];
        for n in [3, CAMERA_CHUNK_PIXELS + 3, PARALLEL_CAMERA_PIXELS + 3] {
            let mut input = Vec::with_capacity(n * 3);
            for channel in 0..3 {
                for i in 0..n {
                    input.push(((i % 23) as f32 - 11.0) * (channel as f32 + 0.375));
                }
            }
            input[0] = f32::MIN_POSITIVE;
            input[n + 1] = -1e30;
            input[3 * n - 1] = 1e-30;
            let expected = scalar_camera_reference(input.clone(), n, &matrix);
            convert_camera_planes(&mut input, n, &matrix, &AtomicBool::new(false)).unwrap();
            assert!(input.iter().all(|value| value.is_finite()));
            assert!(
                input
                    .iter()
                    .zip(&expected)
                    .all(|(actual, expected)| actual.to_bits() == expected.to_bits()),
                "camera conversion differs at {n} pixels"
            );
        }
    }

    #[test]
    fn camera_conversion_rejects_overflow_and_cancellation_without_adoption() {
        let n = PARALLEL_CAMERA_PIXELS + 3;
        let mut planes = vec![1.0; 3 * n];
        planes[n - 1] = f32::MAX;
        let matrix = [[2.0, 0.0, 0.0, 0.0]; 3];
        let error =
            convert_camera_planes(&mut planes, n, &matrix, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedColor);
        assert_eq!(
            error.detail,
            "RAW color conversion produced a non-finite value"
        );

        // A non-finite value arriving from development or the DNG corrections is caught here,
        // even where its matrix coefficient is zero.
        for bad in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            for channel in 0..3 {
                let mut planes = vec![0.5; 3];
                planes[channel] = bad;
                let identity = [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ];
                let error =
                    convert_camera_planes(&mut planes, 1, &identity, &AtomicBool::new(false))
                        .unwrap_err();
                assert_eq!(error.kind, ErrorKind::UnsupportedColor);
            }
        }

        let cancel = AtomicBool::new(true);
        let mut unchanged = vec![1.0; 3 * n];
        let error = convert_camera_planes(&mut unchanged, n, &matrix, &cancel).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert!(unchanged.iter().all(|value| *value == 1.0));
        assert_eq!(
            LinearImage::new(1, 1, vec![1.0, f32::NAN, 1.0])
                .unwrap_err()
                .kind,
            ErrorKind::Validation,
            "public input still checks every value"
        );
    }

    /// Diagnostic only: prepares one real RAW outside the clock, then times the production
    /// converter and private adoption separately on cloned, already-developed planes. The clone,
    /// hash and teardown are outside both timings. Set LIGHTWELL_RAW_MATRIX_SOURCE to a RAW path.
    #[test]
    #[ignore]
    fn camera_conversion_photo_timing() {
        let path = std::env::var("LIGHTWELL_RAW_MATRIX_SOURCE").expect("RAW source path");
        let samples: usize = std::env::var("LIGHTWELL_RAW_MATRIX_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30);
        let cancel = AtomicBool::new(false);
        let sensor = RawSource::decode(Arc::from(std::fs::read(path).unwrap()), &cancel).unwrap();
        let rgb = sensor
            .develop(sensor.metadata().as_shot_gains, &cancel)
            .unwrap();
        let matrix = &sensor.metadata().rgb_cam;
        let n = rgb.plane_len();
        let mut rows = Vec::with_capacity(samples);
        let mut expected_hash = None;
        for trial in 0..=samples {
            let mut planes = rgb.data.clone();
            let start = std::time::Instant::now();
            convert_camera_planes(&mut planes, n, matrix, &cancel).unwrap();
            let convert_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            let image = LinearImage::from_validated_planes(
                rgb.width,
                rgb.height,
                planes,
                "raw-matrix-timing".into(),
            )
            .unwrap();
            let adopt_ms = start.elapsed().as_secs_f64() * 1000.0;
            std::hint::black_box(&image);
            let mut hash = Sha256::new();
            for value in image.planes() {
                hash.update(value.to_bits().to_le_bytes());
            }
            let hash = format!("{:x}", hash.finalize());
            if let Some(expected) = &expected_hash {
                assert_eq!(&hash, expected);
            } else {
                expected_hash = Some(hash);
            }
            if trial > 0 {
                rows.push(json!({"convert_ms":convert_ms,"adopt_ms":adopt_ms}));
            }
        }
        println!(
            "{}",
            json!({
                "camera":sensor.metadata().model,
                "width":rgb.width,
                "height":rgb.height,
                "samples":rows,
                "output_sha256":expected_hash,
                "explicit_scratch_bytes":0,
                "scope":"Production camera conversion and validated adoption; RAW decode/development, clone, hash and drop excluded"
            })
        );
    }

    /// The production preparation of each supplied RAW file — decode, development, DNG
    /// corrections and the camera conversion — at its as-shot gains and at a custom white balance,
    /// and the sensor neutral picker over a grid of upright points, keep the bits they had before
    /// the RAW preparation clean-ups. The digests were captured on the owner's M4; elsewhere the
    /// test prints them. Set `LIGHTWELL_RAW_OWNER_DIR` to the directory holding the files and run
    /// in release:
    ///
    /// ```text
    /// LIGHTWELL_RAW_OWNER_DIR=/path/to/raw cargo test --release -p lightwell-core --lib \
    ///   supplied_raw_preparation -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires the supplied RAW files; run in release"]
    fn supplied_raw_preparation_keeps_its_pinned_bits() {
        let owner = std::env::var("LIGHTWELL_RAW_OWNER_DIR").expect("RAW fixture directory");
        let digest = |values: &mut dyn Iterator<Item = u32>| {
            let mut hash = Sha256::new();
            for value in values {
                hash.update(value.to_le_bytes());
            }
            format!("{:x}", hash.finalize())
        };
        let mut observed = Vec::new();
        for name in ["nikon_z6.NEF", "fujifilm_x100vi.RAF", "mavic_air_2s.DNG"] {
            let bytes = std::fs::read(format!("{owner}/{name}")).expect("read supplied RAW");
            let fingerprint = format!("{:x}", Sha256::digest(&bytes));
            let cancel = AtomicBool::new(false);
            let prepared = RawPrepared::decode(bytes, fingerprint.clone(), None, &cancel).unwrap();
            let as_shot = prepared.linear.as_ref().unwrap();
            let as_shot_planes = digest(&mut as_shot.planes().iter().map(|v| v.to_bits()));
            let gains = prepared.gains;
            let custom = RawPrepared::develop(
                prepared.sensor.clone(),
                fingerprint,
                [gains[0] * 1.15, 1.0, gains[2] * 0.85],
                &cancel,
            )
            .unwrap();
            let custom_planes = digest(
                &mut custom
                    .linear
                    .as_ref()
                    .unwrap()
                    .planes()
                    .iter()
                    .map(|v| v.to_bits()),
            );
            let (width, height) = PreparedSource::Raw(prepared.clone()).dimensions();
            let mut picks = Vec::new();
            for row in 0..12 {
                for column in 0..16 {
                    let x = (2 * column + 1) * width / 32;
                    let y = (2 * row + 1) * height / 24;
                    match neutral_at(&prepared, x, y) {
                        Ok(gains) => picks.extend(gains.map(f32::to_bits)),
                        Err(error) => {
                            picks.push(u32::MAX);
                            picks.extend(error.kind.code().bytes().map(u32::from));
                        }
                    }
                }
            }
            let neutral = digest(&mut picks.into_iter());
            println!(
                "{}",
                json!({"source": name, "as_shot": as_shot_planes, "custom": custom_planes, "neutral": neutral})
            );
            observed.push([as_shot_planes, custom_planes, neutral]);
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(
            observed,
            [
                [
                    "8cb2c7a72d150de508fc801a6945e3d8a6317f149dfe7540dab646d8960b4c2f",
                    "a921eb188f52eb40a2cd3f33b1eadce18d5c4176cff4dae0c392ebeb4de46f1e",
                    "8786b2a8eac76f9d5c0d84cbe76538a39052a477829b8026cf1381e1b74f5e73",
                ],
                [
                    "cd03c8a9f1ffecb61e6eb727e1cc8193c3e9d11e0be89d0f007fee9fa725390b",
                    "db1e6e74a5243b569e5ac01b77799a9c820eb42014546aadaa333ac6e2ff30de",
                    "fb954d5ec310f1f8c2044c716c4c713c0185fc70f35b090e1f81ba9adb273372",
                ],
                [
                    "3c18cd41eada46ea4defa9853e3eb4912748ddd5df137abb9ece6bfad8880992",
                    "34c57a2fce8b02b77c1d8440dc62b13bfde7cf690a138c4c089eafe0cf1bc5af",
                    "2367f81e4ede5c1586c5b73b44422f5d7b1d035f681ad4414f480790528b407b",
                ],
            ]
        );
    }

    /// How far one 8-bit rendition is from another, and where the differences sit.
    fn compare(approximate: &crate::Raster, exact: &crate::Raster) -> (Value, Vec<u8>) {
        assert_eq!(
            (approximate.width, approximate.height),
            (exact.width, exact.height)
        );
        let (width, height) = (exact.width as usize, exact.height as usize);
        let pixels = width * height;
        let luma = |rgba: &[u8], index: usize| -> f64 {
            let pixel = &rgba[index * 4..index * 4 + 3];
            0.2126 * f64::from(pixel[0])
                + 0.7152 * f64::from(pixel[1])
                + 0.0722 * f64::from(pixel[2])
        };
        let mut sums = [0.0_f64; 3];
        let mut worst = vec![0_u8; pixels];
        let (mut over, mut edge_over, mut edges) = (0_usize, 0_usize, 0_usize);
        let (mut highlight_over, mut highlights) = (0_usize, 0_usize);
        let (mut saturated_over, mut saturated) = (0_usize, 0_usize);
        let (mut dark_over, mut dark) = (0_usize, 0_usize);
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                let a = &approximate.rgba[index * 4..index * 4 + 3];
                let e = &exact.rgba[index * 4..index * 4 + 3];
                let mut largest = 0_u8;
                for channel in 0..3 {
                    let difference = a[channel].abs_diff(e[channel]);
                    sums[channel] += f64::from(difference);
                    largest = largest.max(difference);
                }
                worst[index] = largest;
                // A local luma gradient of the exact frame over 24 codes is an edge; a channel at
                // 250 or above in either frame is a highlight; a channel spread of 128 or more is a
                // saturated colour; a luma under 16 is a shadow.
                let gradient = if x > 0 && y > 0 && x + 1 < width && y + 1 < height {
                    (luma(&exact.rgba, index + 1) - luma(&exact.rgba, index - 1)).abs()
                        + (luma(&exact.rgba, index + width) - luma(&exact.rgba, index - width))
                            .abs()
                } else {
                    0.0
                };
                let is_edge = gradient > 24.0;
                let is_highlight = e.iter().chain(a.iter()).any(|value| *value >= 250);
                let spread = e.iter().max().unwrap() - e.iter().min().unwrap();
                let is_saturated = spread >= 128;
                let is_dark = luma(&exact.rgba, index) < 16.0;
                edges += usize::from(is_edge);
                highlights += usize::from(is_highlight);
                saturated += usize::from(is_saturated);
                dark += usize::from(is_dark);
                if largest > 2 {
                    over += 1;
                    edge_over += usize::from(is_edge);
                    highlight_over += usize::from(is_highlight);
                    saturated_over += usize::from(is_saturated);
                    dark_over += usize::from(is_dark);
                }
            }
        }
        let mut ranked = worst.clone();
        ranked.sort_unstable();
        let share = |count: usize, of: usize| {
            if of == 0 {
                0.0
            } else {
                count as f64 / of as f64
            }
        };
        let summary = json!({
            "dimensions": [width, height],
            "mean_abs_codes": sums.map(|sum| sum / pixels as f64),
            "p99_max_channel_codes": ranked[(pixels * 99 / 100).min(pixels - 1)],
            "p999_max_channel_codes": ranked[(pixels * 999 / 1000).min(pixels - 1)],
            "max_codes": ranked[pixels - 1],
            "share_over_2_codes": share(over, pixels),
            "over_2_by_region": {
                "edges": {"share_of_over": share(edge_over, over), "share_of_frame": share(edges, pixels)},
                "highlights": {"share_of_over": share(highlight_over, over), "share_of_frame": share(highlights, pixels)},
                "saturated": {"share_of_over": share(saturated_over, over), "share_of_frame": share(saturated, pixels)},
                "shadows": {"share_of_over": share(dark_over, over), "share_of_frame": share(dark, pixels)},
            },
        });
        (summary, worst)
    }

    fn save_gray(path: &std::path::Path, width: u32, height: u32, worst: &[u8]) {
        // Eight times the difference, so a 2-code difference is a visible 16 and 32 codes is white.
        let pixels: Vec<u8> = worst.iter().map(|value| value.saturating_mul(8)).collect();
        image::save_buffer(path, &pixels, width, height, image::ColorType::L8).unwrap();
    }

    fn save_rgba(path: &std::path::Path, raster: &crate::Raster) {
        image::save_buffer(
            path,
            &raster.rgba,
            raster.width,
            raster.height,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    /// The accuracy of the drafted white-balance approximation on a real RAW file: the planes
    /// developed at the camera's as-shot gains, rendered through `W` for a custom temperature and
    /// tint, against an exact redevelopment of the mosaic at the same gains, both rendered to 8-bit
    /// sRGB at a display proxy size and at full size. It prints one JSON document and, with
    /// `LIGHTWELL_WB_OUT` set to a directory, writes each difference image (eight times the largest
    /// channel difference) and both proxy renditions there. It is a measurement, not a gate:
    ///
    /// ```text
    /// LIGHTWELL_RAW_FIXTURE=/path/to/file.NEF LIGHTWELL_WB_OUT=/tmp/wb \
    ///   cargo test --release -p lightwell-core --lib source::tests -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires a photo-sized RAW fixture and is a measurement, not a gate"]
    fn measure_the_white_balance_approximation_against_redevelopment() {
        let path =
            std::path::PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let out = std::env::var("LIGHTWELL_WB_OUT")
            .ok()
            .map(std::path::PathBuf::from);
        if let Some(out) = &out {
            std::fs::create_dir_all(out).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(&bytes));
        let cancel = AtomicBool::new(false);
        let as_shot = RawPrepared::decode(bytes, fingerprint.clone(), None, &cancel).unwrap();
        let metadata = as_shot.sensor.metadata().clone();
        let camera = metadata
            .rgb_cam
            .map(|row| [row[0], row[1], row[2]].map(f64::from));
        let developed = as_shot.linear.clone().unwrap();
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe::default();
        let bounds = ProxyBounds {
            width: 2400,
            height: 1600,
        };
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let mut cases = Vec::new();
        for (kelvin, tint) in [
            (3200.0, 0.0),
            (5000.0, 20.0),
            (8000.0, -20.0),
            (6504.0, 0.0),
        ] {
            let target = gains_from_temperature_tint(kelvin, tint, metadata.cam_xyz).unwrap();
            let balance =
                WhiteBalanceApproximation::between(camera, as_shot.gains, target).unwrap();
            let started = std::time::Instant::now();
            let exact =
                RawPrepared::develop(as_shot.sensor.clone(), fingerprint.clone(), target, &cancel)
                    .unwrap();
            let redevelop_ms = started.elapsed().as_secs_f64() * 1000.0;
            let approximate = PreviewSource::Raw {
                image: developed.clone(),
                settings: LinearSettings {
                    exposure_ev: 0.0,
                    white_balance: Some(balance),
                },
            };
            let reference = PreviewSource::Raw {
                image: exact.linear.clone().unwrap(),
                settings: LinearSettings::default(),
            };
            let snapshot = SnapshotId::new();
            let full_approximate = approximate
                .render(&registry, snapshot.clone(), &recipe)
                .unwrap();
            let full_exact = reference
                .render(&registry, snapshot.clone(), &recipe)
                .unwrap();
            let (full, full_worst) = compare(&full_approximate, &full_exact);
            let plan = approximate
                .proxy_plan(&registry, &recipe, bounds)
                .unwrap()
                .expect("a photo is larger than the display");
            let proxy_approximate = approximate
                .proxy(plan)
                .unwrap()
                .render(&registry, snapshot.clone(), &recipe)
                .unwrap();
            let proxy_exact = reference
                .proxy(plan)
                .unwrap()
                .render(&registry, snapshot, &recipe)
                .unwrap();
            let (proxy, proxy_worst) = compare(&proxy_approximate, &proxy_exact);
            // How far the as-shot frame itself is from the target, for scale: what a drag shows
            // when nothing is previewed at all.
            let unchanged = render_linear(
                &registry,
                &developed,
                SnapshotId::new(),
                &recipe,
                LinearSettings::default(),
            )
            .unwrap();
            let (as_shot_against_target, _) = compare(&unchanged, &full_exact);
            if let Some(out) = &out {
                let name = format!("{stem}-{kelvin:.0}K-{tint:+.0}");
                save_gray(
                    &out.join(format!("{name}-proxy-diff.png")),
                    proxy_approximate.width,
                    proxy_approximate.height,
                    &proxy_worst,
                );
                save_gray(
                    &out.join(format!("{name}-full-diff.png")),
                    full_approximate.width,
                    full_approximate.height,
                    &full_worst,
                );
                save_rgba(
                    &out.join(format!("{name}-proxy-approximate.png")),
                    &proxy_approximate,
                );
                save_rgba(&out.join(format!("{name}-proxy-exact.png")), &proxy_exact);
            }
            cases.push(json!({
                "kelvin": kelvin,
                "tint": tint,
                "as_shot_gains": as_shot.gains,
                "target_gains": target,
                "matrix": balance.matrix(),
                "redevelop_ms": redevelop_ms,
                "proxy": proxy,
                "full": full,
                "as_shot_frame_against_target_full": {
                    "mean_abs_codes": as_shot_against_target["mean_abs_codes"],
                    "share_over_2_codes": as_shot_against_target["share_over_2_codes"],
                },
            }));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": path,
                "model": metadata.model,
                "mode": metadata.mode,
                "cases": cases,
            }))
            .unwrap()
        );
    }
}

/// The JPEG decode's input contract: SOI/EOI and marker bounds, declared sizes, orientation and
/// the encoded-byte limit, plus proof that a decode returns the very frame it wrote pixels into.
#[cfg(test)]
mod jpeg_tests {
    use super::*;
    use crate::{
        AssetId, Layer, ModuleRegistry, Orientation, RECIPE_FORMAT, Recipe, Snapshot, SnapshotId,
        Transform, render::testing::render,
    };

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0")
            .join(name)
    }

    #[test]
    fn oversized_jpeg_is_rejected_from_file_length_before_buffer_allocation() {
        use std::io::{Seek, SeekFrom, Write};
        let path = std::env::temp_dir().join(format!(
            "lightwell-oversized-jpeg-{}-{}.jpg",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_JPEG_BYTES as u64 + 1).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[0xff, 0xd8]).unwrap();
        file.flush().unwrap();
        let mut opened = std::fs::File::open(&path).unwrap();
        let error = read_bounded_file(&mut opened).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn orientations_and_preservation() {
        let permutations = [
            [0, 1, 2, 3],
            [1, 0, 3, 2],
            [3, 2, 1, 0],
            [2, 3, 0, 1],
            [0, 2, 1, 3],
            [2, 0, 3, 1],
            [3, 1, 2, 0],
            [1, 3, 0, 2],
        ];
        let colors: [[u8; 3]; 4] = [[220, 35, 45], [35, 190, 65], [40, 70, 220], [235, 195, 30]];
        for orientation in 1..=8 {
            let path = fixture(&format!("orientation-{orientation}.jpg"));
            let original = std::fs::read(&path).unwrap();
            let source = open_source(&path).unwrap();
            assert_eq!(
                (source.width, source.height),
                if orientation >= 5 {
                    (320, 480)
                } else {
                    (480, 320)
                }
            );
            for (index, (x, y)) in [(1, 1), (3, 1), (1, 3), (3, 3)].iter().enumerate() {
                let offset =
                    (((source.height * y / 4) * source.width + source.width * x / 4) * 4) as usize;
                for (actual, expected) in source.rgba[offset..offset + 3]
                    .iter()
                    .zip(colors[permutations[orientation - 1][index]])
                {
                    assert!(actual.abs_diff(expected) <= 5);
                }
            }
            assert_eq!(std::fs::read(path).unwrap(), original);
        }
    }

    #[test]
    fn pixel_edits_use_upright_coordinates_for_every_exif_orientation() {
        for orientation in 1..=8 {
            let path = fixture(&format!("orientation-{orientation}.jpg"));
            let bytes = std::fs::read(&path).unwrap();
            let source = open_source(&path).unwrap();
            let snapshot = Snapshot::original(AssetId::new())
                .append(Layer::pixel(source.width - 1, source.height - 1, [1, 2, 3]))
                .unwrap();
            let raster = render(
                &ModuleRegistry::builtin(),
                &source,
                snapshot.id,
                &snapshot.recipe,
            )
            .unwrap();
            assert_eq!(
                raster.pixel(source.width - 1, source.height - 1),
                Some([1, 2, 3, 255])
            );
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    /// Every sequence of up to three transform actions on every EXIF orientation, including the
    /// mirrored ones, renders byte-identically whether the actions are separate single-action
    /// orientation layers or one layer holding their composition. The source is untouched.
    #[test]
    fn composed_and_separate_orientations_agree_on_every_exif_orientation() {
        let registry = ModuleRegistry::builtin();
        let transforms = [
            Transform::RotateLeft,
            Transform::RotateRight,
            Transform::MirrorHorizontal,
            Transform::FlipVertical,
        ];
        for orientation in 1..=8 {
            let path = fixture(&format!("orientation-{orientation}.jpg"));
            let bytes = std::fs::read(&path).unwrap();
            let source = open_source(&path).unwrap();
            let rendered = |layers: Vec<Layer>| {
                render(
                    &registry,
                    &source,
                    SnapshotId::new(),
                    &Recipe {
                        format: RECIPE_FORMAT,
                        layers,
                        masks: Vec::new(),
                        ..Recipe::default()
                    },
                )
                .unwrap()
            };
            // Every sequence of one and two actions on every fixture, and every sequence of three
            // on the mirrored ones, where a reflection composed the wrong way round would show.
            let mut sequences: Vec<Vec<Transform>> = Vec::new();
            for first in transforms {
                sequences.push(vec![first]);
                for second in transforms {
                    sequences.push(vec![first, second]);
                    if matches!(orientation, 2 | 4 | 5 | 7) {
                        for third in transforms {
                            sequences.push(vec![first, second, third]);
                        }
                    }
                }
            }
            for actions in sequences {
                let separate: Vec<Layer> = actions
                    .iter()
                    .map(|transform| Layer::orientation(Orientation::of(*transform)))
                    .collect();
                let composed = actions
                    .iter()
                    .fold(Orientation::NEUTRAL, |state, transform| {
                        state.then(*transform)
                    });
                let stepwise = rendered(separate);
                let collapsed = rendered(vec![Layer::orientation(composed)]);
                assert_eq!(
                    (stepwise.width, stepwise.height),
                    (collapsed.width, collapsed.height),
                    "orientation {orientation}: {actions:?}"
                );
                assert_eq!(
                    stepwise.rgba, collapsed.rgba,
                    "orientation {orientation}: {actions:?}"
                );
            }
            assert_eq!(std::fs::read(path).unwrap(), bytes, "source unchanged");
        }
    }

    #[test]
    fn input_contract() {
        for name in ["srgb.jpg", "portrait.jpg", "greyscale.jpg"] {
            assert!(open_source(&fixture(name)).is_ok(), "{name}");
        }
        for (name, code) in [
            ("invalid.jpg", "invalid-input"),
            ("truncated.jpg", "invalid-input"),
            ("oversized.jpg", "resource-limit"),
            ("cmyk.jpg", "unsupported-color"),
            ("invalid-profile.jpg", "unsupported-profile"),
            ("missing.jpg", "read-error"),
        ] {
            assert!(
                open_source(&fixture(name)).err().unwrap().kind.code() == code,
                "{name}"
            );
        }
    }

    #[test]
    fn malformed_headers_never_panic_or_allocate_from_dimensions() {
        let valid = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"),
        )
        .unwrap();
        for end in 0..valid.len().min(1024) {
            assert!(header(&valid[..end]).is_err());
        }
        for size in [0u16, 1, 2, 7, u16::MAX] {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0];
            bytes.extend(size.to_be_bytes());
            bytes.extend([8, 0xff, 0xff, 0xff, 0xff, 3, 0xff, 0xd9]);
            let _ = header(&bytes);
        }
    }

    #[test]
    fn readonly_unicode_source_is_supported_and_preserved() {
        let dir =
            std::env::temp_dir().join(format!("lightwell read only ü {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("photo ü.jpg");
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"),
        )
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let original = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&path, readonly).unwrap();
        assert!(open_source(&path).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::set_permissions(&path, original).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG decode allocates its RGBA frame once and writes straight into it: no `into_rgba8()`
    /// buffer that a second allocation then copies into the `Arc<[u8]>` a [`SourceImage`] holds.
    /// Proved the way the render tests prove a pass writes the frame it returns
    /// ([`crate::render::frame_writes`]): the address [`crate::render::frame_mut`] hands out while
    /// decoding is the address the returned `SourceImage.rgba` itself points at.
    #[test]
    fn decode_writes_the_frame_it_returns() {
        let bytes = std::fs::read(fixture("orientation-1.jpg")).unwrap();
        let (source, written) =
            crate::render::frame_writes::record(|| open_source_bytes(bytes).unwrap());
        assert_eq!(
            written.last(),
            Some(&(source.rgba.as_ptr() as usize)),
            "the decoded source is the frame written last, not a copy of it"
        );
    }
}
