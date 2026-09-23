//! UI-independent JPEG decoding, non-destructive editing state, rendering and the JSON owner API.
pub mod activity;
pub mod analysis;
mod api;
#[cfg(test)]
mod command_contracts;
mod draft;
mod editor;
mod error;
mod model;
mod modules;
mod presets;
mod preview;
mod profile;
mod proxy;
mod render;
pub mod resources;
mod source;
pub use activity::{Activity, ActivityBoard, ActivitySnapshot, ActivitySpec};
pub use api::*;
pub use draft::Draft;
pub use editor::*;
pub use error::{Error, ErrorKind};
use image::{ImageDecoder, ImageReader, Limits};
pub use model::*;
pub use modules::*;
pub use presets::*;
pub use preview::*;
pub use proxy::{ProxyBounds, ProxyCache, ProxyIdentity, ProxyKey, ProxyPlan};
pub use render::{
    Cancel, ContentPoint, LinearImage, LinearSettings, Raster, Sample, ScratchBudget,
    SpatialBudget, WhiteBalanceApproximation, cached_estimates, clear_estimates, extents, locate,
    render, render_cancellable, render_linear, render_linear_cancellable, sample, sample_linear,
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::Instant,
};

/// Wall-clock stages on the image worker; excludes scheduling and GPU upload.
#[derive(Clone, Debug, Default)]
pub struct DecodeTimings {
    pub read_ms: f64,
    pub validate_ms: f64,
    pub pixels_ms: f64,
    pub orient_ms: f64,
    pub resize_ms: f64,
    pub rgba_ms: f64,
}

pub struct Photo {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub preview_width: u32,
    pub preview_height: u32,
    pub decode_ms: f64,
    pub timings: DecodeTimings,
    pub orientation: u8,
}

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

fn read_bounded(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file =
        File::open(path).map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
    read_bounded_file(&mut file)
}

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
        128 * 1024 * 1024
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
    validate_ms: f64,
    pixels_ms: f64,
    orient_ms: f64,
}

fn ms(from: Instant, to: Instant) -> f64 {
    (to - from).as_secs_f64() * 1000.0
}

/// Validate the supported JPEG subset, decode within fixed limits and orient once to upright pixels.
fn decode_upright(bytes: Vec<u8>) -> Result<Decoded, Error> {
    let start = Instant::now();
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
        profile::check(&profile, components)?;
    }
    let orientation = decoder
        .orientation()
        .map_err(|_| decode_error("orientation"))?;
    let validate_done = Instant::now();
    let mut upright =
        image::DynamicImage::from_decoder(decoder).map_err(|_| decode_error("decode"))?;
    let pixels_done = Instant::now();
    upright.apply_orientation(orientation);
    let orient_done = Instant::now();
    Ok(Decoded {
        upright,
        orientation: orientation.to_exif(),
        validate_ms: ms(start, validate_done),
        pixels_ms: ms(validate_done, pixels_done),
        orient_ms: ms(pixels_done, orient_done),
    })
}

pub fn open(path: &Path) -> Result<Photo, Error> {
    let start = Instant::now();
    let bytes = read_bounded(path)?;
    let read_done = Instant::now();
    let decoded = decode_upright(bytes)?;
    let mut upright = decoded.upright;
    let width = upright.width();
    let height = upright.height();
    let resize_start = Instant::now();
    // S0 displays Fit only; cap upload work and textures on the decoding worker.
    if width > 4096 || height > 4096 {
        upright = upright.resize(4096, 4096, image::imageops::FilterType::Triangle);
    }
    let resize_done = Instant::now();
    let rgba = upright.into_rgba8();
    let rgba_done = Instant::now();
    Ok(Photo {
        width,
        height,
        preview_width: rgba.width(),
        preview_height: rgba.height(),
        rgba: rgba.into_raw(),
        orientation: decoded.orientation,
        decode_ms: start.elapsed().as_secs_f64() * 1000.0,
        timings: DecodeTimings {
            read_ms: ms(start, read_done),
            validate_ms: decoded.validate_ms,
            pixels_ms: decoded.pixels_ms,
            orient_ms: decoded.orient_ms,
            resize_ms: ms(resize_start, resize_done),
            rgba_ms: ms(resize_done, rgba_done),
        },
    })
}

/// Decode the complete upright source once for non-destructive recipe evaluation.
pub fn open_source(path: &Path) -> Result<SourceImage, Error> {
    let mut file =
        File::open(path).map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
    open_source_file(&mut file)
}

/// Hash and decode one bounded snapshot read from an already opened handle.
pub(crate) fn open_source_file(file: &mut File) -> Result<SourceImage, Error> {
    open_source_bytes(read_bounded_file(file)?)
}

pub(crate) fn open_source_bytes(bytes: Vec<u8>) -> Result<SourceImage, Error> {
    let fingerprint = format!("{:x}", Sha256::digest(&bytes));
    let decoded = decode_upright(bytes)?;
    let rgba = decoded.upright.into_rgba8();
    Ok(SourceImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw().into(),
        fingerprint,
        orientation: decoded.orientation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        file.set_len(128 * 1024 * 1024 + 1).unwrap();
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
            let photo = open(&path).unwrap();
            assert_eq!(
                (photo.width, photo.height),
                if orientation >= 5 {
                    (320, 480)
                } else {
                    (480, 320)
                }
            );
            for (index, (x, y)) in [(1, 1), (3, 1), (1, 3), (3, 3)].iter().enumerate() {
                let offset =
                    (((photo.height * y / 4) * photo.width + photo.width * x / 4) * 4) as usize;
                for (actual, expected) in photo.rgba[offset..offset + 3]
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
            assert!(open(&fixture(name)).is_ok(), "{name}");
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
                open(&fixture(name)).err().unwrap().kind.code() == code,
                "{name}"
            );
        }
    }
}

/// One active decode, one replaceable pending path, one bounded result.
/// Polling is needed only while loading; dropping the viewer never joins a decode on the UI thread.
#[derive(Default)]
pub struct Loader {
    generation: u64,
    active: Option<(u64, std::sync::mpsc::Receiver<Result<Photo, Error>>)>,
    pending: Option<std::path::PathBuf>,
}
impl Loader {
    pub fn request(&mut self, path: std::path::PathBuf) {
        self.generation += 1;
        if self.active.is_some() {
            self.pending = Some(path);
        } else {
            self.start(path);
        }
    }
    fn start(&mut self, path: std::path::PathBuf) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = sender.send(open(&path));
        });
        self.active = Some((self.generation, receiver));
    }
    pub fn loading(&self) -> bool {
        self.active.is_some()
    }
    pub fn poll(&mut self) -> Option<Result<Photo, Error>> {
        let (generation, receiver) = self.active.as_ref()?;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(_) => Err(Error::new(
                ErrorKind::Internal,
                "decode worker disconnected",
            )),
        };
        let current = *generation == self.generation;
        self.active = None;
        if let Some(path) = self.pending.take() {
            self.start(path);
        }
        if current { Some(result) } else { None }
    }
}

#[cfg(test)]
mod loader_tests {
    use super::*;
    #[test]
    fn newest_request_wins_with_bounded_pending_work() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0");
        let mut loader = Loader::default();
        loader.request(base.join("orientation-1.jpg"));
        loader.request(base.join("invalid.jpg"));
        loader.request(base.join("orientation-6.jpg"));
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = loader.poll() {
                let photo =
                    result.expect("latest valid request supersedes pending invalid request");
                assert_eq!((photo.width, photo.height), (320, 480));
                assert!(!loader.loading());
                break;
            }
            assert!(Instant::now() < deadline, "bounded loader did not complete");
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn large_source_produces_bounded_preview_without_changing_dimensions_or_source() {
        let path = std::env::temp_dir().join(format!(
            "lightwell-preview-{}-{}.jpg",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let image = image::RgbImage::from_pixel(5000, 100, image::Rgb([80, 120, 160]));
        image.save(&path).unwrap();
        let source = std::fs::read(&path).unwrap();
        let photo = open(&path).unwrap();
        assert_eq!((photo.width, photo.height), (5000, 100));
        assert_eq!(photo.preview_width, 4096);
        assert!(photo.preview_height <= 4096);
        assert_eq!(
            photo.rgba.len(),
            (photo.preview_width * photo.preview_height * 4) as usize
        );
        assert_eq!(std::fs::read(&path).unwrap(), source);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
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
    fn dropping_an_active_loader_does_not_join_or_modify_source() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
        let bytes = std::fs::read(&source).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // Deterministically model a worker that has not returned yet.
        let mut loader = Loader {
            generation: 1,
            active: Some((1, rx)),
            pending: Some(source.clone()),
        };
        loader.request(source.clone());
        drop(loader);
        assert!(
            tx.send(Err(Error::new(ErrorKind::Internal, "test")))
                .is_err()
        );
        assert_eq!(std::fs::read(source).unwrap(), bytes);
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
        assert!(open(&path).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::set_permissions(&path, original).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
