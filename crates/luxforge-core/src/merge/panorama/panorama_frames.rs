//! Panorama frames against independent references: JPEG frames against the reference crate's f64
//! sRGB transfer and a Display P3 matrix derived from the primaries' chromaticities, RAW frames through the
//! production development of a synthetic DNG against that development itself and against the
//! reference crate's lens warp, and the frame path's bounds, cancellation and plane-gate
//! independence.
use super::*;
use crate::{
    ErrorKind, LinearImage,
    catalog_types::{CameraBody, Exposure, HeaderMetadata},
    merge::{
        hdr::{FrameMetadata, Recording, SensorLayout},
        job::{Developer, Limits, SharedDevelopment, settings},
    },
    source::PlaneGate,
};
use luxforge_raw::OpticalLedger;
use luxforge_reference::geometry as reference;
use luxforge_test_macros::slow;
use luxforge_testbase::paths::{temp_dir, temp_path};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{Mutex, Weak},
};

use crate::merge::hdr::hdr_bracket;

/// The declared tolerance of a JPEG frame's linear sRGB values against the independent transfer
/// and matrix reference, absolute: the ICC colorants' s15Fixed16 encoding, the D50 connection
/// space's adaptation and the f32 tone-curve tables together stay well inside it (about 2e-4 for
/// Display P3, 6e-5 for a tagged sRGB file and 3e-8 for an untagged one, measured here).
const JPEG_TOLERANCE: f64 = 5e-4;
/// The declared tolerance of a lens-baked RAW frame against the reference crate's f64 warp of the
/// same development, relative to the larger of one and the value: the f32 samples and the f64
/// mapping's different but equivalent evaluation order (about 6e-8, measured here).
const WARP_TOLERANCE: f64 = 1e-6;

fn accounting() -> Accounting {
    Accounting::of(&Limits::of(crate::catalog_types::MergeKind::Hdr))
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// The spool's pixels, as written: three samples and a level each.
fn spooled(path: &Path) -> Vec<([f32; 3], SensorLevel)> {
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len() % SPOOL_PIXEL as usize, 0);
    bytes
        .chunks_exact(SPOOL_PIXEL as usize)
        .map(|pixel| {
            let sample = |at: usize| f32::from_le_bytes(pixel[at..at + 4].try_into().unwrap());
            (
                [sample(0), sample(4), sample(8)],
                SensorLevel(u16::from_le_bytes([pixel[12], pixel[13]])),
            )
        })
        .collect()
}

// ---- JPEG frames ----------------------------------------------------------------------------

/// A 128 × 96 test chart of flat 16 × 16 patches (whole JPEG blocks, so a quality-100 4:4:4
/// encode keeps each patch's codes): neutrals from black to white, saturated primaries and
/// secondaries, and patches with one channel at 255.
fn chart() -> (u32, u32, Vec<u8>) {
    let (width, height) = (128_u32, 96_u32);
    let patches: Vec<[u8; 3]> = vec![
        [0, 0, 0],
        [1, 1, 1],
        [10, 10, 10],
        [50, 50, 50],
        [118, 118, 118],
        [200, 200, 200],
        [254, 254, 254],
        [255, 255, 255],
        [254, 0, 0],
        [0, 254, 0],
        [0, 0, 254],
        [254, 254, 0],
        [0, 254, 254],
        [254, 0, 254],
        [255, 30, 30],
        [30, 255, 30],
        [30, 30, 255],
        [255, 128, 0],
        [12, 200, 90],
        [180, 40, 220],
        [90, 60, 40],
        [240, 210, 180],
        [20, 60, 140],
        [128, 128, 64],
    ];
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let patch = patches[((y / 16) * (width / 16) + x / 16) as usize % patches.len()];
            rgba.extend_from_slice(&[patch[0], patch[1], patch[2], 255]);
        }
    }
    (width, height, rgba)
}

fn encode_chart(icc: Option<&[u8]>) -> Vec<u8> {
    let (width, height, rgba) = chart();
    let mut out = Vec::new();
    luxforge_jpeg::encode(
        &mut out,
        width,
        height,
        &rgba,
        &luxforge_jpeg::Settings {
            quality: 100,
            chroma: (1, 1),
            pixels_per_inch: None,
            segments: &[],
            icc,
        },
        &mut |_| Ok::<(), luxforge_jpeg::JpegError>(()),
    )
    .unwrap();
    out
}

/// The JPEG's codes as the shared codec decodes them; the colour reference below is independent.
fn decoded_codes(bytes: &[u8]) -> Vec<[u8; 3]> {
    let mut decoder = luxforge_jpeg::Decoder::new(bytes, crate::source::JPEG_LIMITS).unwrap();
    let mut rgba = vec![0; decoder.width() as usize * decoder.height() as usize * 4];
    decoder.read_rows(&mut rgba).unwrap();
    decoder.finish().unwrap();
    rgba.chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

fn inverse(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let cofactor = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let determinant: f64 = (0..3).map(|c| m[0][c] * cofactor(0, c)).sum();
    std::array::from_fn(|r| std::array::from_fn(|c| cofactor(c, r) / determinant))
}

fn multiply(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

/// The RGB-to-XYZ matrix of primaries and a white point given as xy chromaticities.
fn rgb_to_xyz(primaries: [[f64; 2]; 3], white: [f64; 2]) -> [[f64; 3]; 3] {
    let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
    let columns = primaries.map(xyz);
    let p: [[f64; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| columns[c][r]));
    let w = xyz(white);
    let inverse = inverse(p);
    let s: [f64; 3] = std::array::from_fn(|r| (0..3).map(|k| inverse[r][k] * w[k]).sum());
    std::array::from_fn(|r| std::array::from_fn(|c| p[r][c] * s[c]))
}

const D65: [f64; 2] = [0.3127, 0.3290];
const SRGB_PRIMARIES: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
const P3_PRIMARIES: [[f64; 2]; 3] = [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]];

/// Read the JPEG at `path` as a panorama frame and compare every spooled pixel with `reference`
/// of its independently decoded codes; every clipped mark with the codes. Answers the largest
/// difference and how many components were negative.
fn compare_jpeg(path: &Path, reference: impl Fn([u8; 3]) -> [f64; 3]) -> (f64, usize) {
    let before = sha256(path);
    let spool = temp_path("panorama-frame.spool");
    let control = JobControl::new();
    let mut accounting = accounting();
    let frame = read_frame(
        &SharedFrames,
        &FrameRequest {
            path,
            gains: None,
            lens: LensChoice::Detect,
            exposure_scale: 1.0,
            colour: true,
        },
        &spool,
        &control,
        &mut accounting,
    )
    .unwrap();
    assert_eq!(frame.developed.frame.recording, Recording::Jpeg);
    assert!(frame.lens.is_none() && frame.gains.is_none());
    assert_eq!(frame.optics.ledger, SourceOptics::jpeg_ledger());
    assert!(frame.spooled.colour.is_some());
    assert_eq!(accounting.scratch_bytes, 0, "the band is given back");
    let pixels = spooled(&spool);
    let codes = decoded_codes(&fs::read(path).unwrap());
    assert_eq!(pixels.len(), codes.len());
    assert_eq!(frame.spooled.bytes, fs::metadata(&spool).unwrap().len());
    let (mut worst, mut negative, mut clipped) = (0.0_f64, 0, 0);
    for (&(rgb, level), &code) in pixels.iter().zip(&codes) {
        let want = reference(code);
        for c in 0..3 {
            worst = worst.max((f64::from(rgb[c]) - want[c]).abs());
            negative += usize::from(rgb[c] < 0.0);
        }
        assert_eq!(
            level.is_clipped(),
            code.contains(&255),
            "a sample is clipped exactly where an encoded channel is 255: {code:?}"
        );
        clipped += usize::from(level.is_clipped());
    }
    assert_eq!(frame.spooled.clipped, clipped as u64);
    assert!(clipped > 0);
    assert_eq!(sha256(path), before, "the original is byte-identical");
    fs::remove_file(spool).unwrap();
    (worst, negative)
}

#[test]
fn panorama_frames_read_srgb_jpegs_as_the_srgb_transfer() {
    let directory = temp_dir("panorama-frames-srgb");
    let reference = |code: [u8; 3]| code.map(luxforge_reference::srgb::decode);
    for (name, icc) in [
        ("untagged.jpg", None),
        (
            "srgb.jpg",
            Some(moxcms::ColorProfile::new_srgb().encode().unwrap()),
        ),
    ] {
        let path = directory.join(name);
        fs::write(&path, encode_chart(icc.as_deref())).unwrap();
        let (worst, negative) = compare_jpeg(&path, reference);
        assert!(worst <= JPEG_TOLERANCE, "{name}: {worst:e}");
        if icc.is_none() {
            assert_eq!(negative, 0, "{name}: sRGB codes are inside sRGB");
        }
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn panorama_frames_read_display_p3_jpegs_through_their_profile() {
    let directory = temp_dir("panorama-frames-p3");
    let mut p3 = moxcms::ColorProfile::new_display_p3();
    // As Apple's Display P3 profile, with no CICP tag beside its matrix and curves.
    p3.cicp = None;
    let path = directory.join("p3.jpg");
    fs::write(&path, encode_chart(Some(&p3.encode().unwrap()))).unwrap();
    let to_srgb = multiply(
        inverse(rgb_to_xyz(SRGB_PRIMARIES, D65)),
        rgb_to_xyz(P3_PRIMARIES, D65),
    );
    let (worst, negative) = compare_jpeg(&path, |code| {
        let linear = code.map(luxforge_reference::srgb::decode);
        std::array::from_fn(|r| (0..3).map(|k| to_srgb[r][k] * linear[k]).sum())
    });
    assert!(worst <= JPEG_TOLERANCE, "{worst:e}");
    assert!(
        negative > 0,
        "saturated P3 keeps its negative sRGB components"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn panorama_frames_refuse_a_profile_whose_cicp_disagrees() {
    let directory = temp_dir("panorama-frames-cicp");
    let mut conflicting = moxcms::ColorProfile::new_display_p3();
    conflicting.cicp = moxcms::ColorProfile::new_srgb().cicp;
    let path = directory.join("conflicting.jpg");
    fs::write(&path, encode_chart(Some(&conflicting.encode().unwrap()))).unwrap();
    let error = SharedFrames
        .develop(&path, None, &JobControl::new())
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::UnsupportedProfile, "{error:?}");
    // A gamma 2.2 RGB profile, which the editor refuses, is read as it declares.
    let mut gamma = moxcms::ColorProfile::new_adobe_rgb();
    gamma.cicp = None;
    let colour = RgbProfile::of(Some(&gamma.encode().unwrap()), 3).unwrap();
    assert!(
        (f64::from(colour.linear[1][128]) - (128.0_f64 / 255.0).powf(2.199_218_75)).abs() < 1e-4
    );
    assert!(RgbProfile::of(Some(&gamma.encode().unwrap()), 1).is_err());
    fs::remove_dir_all(directory).unwrap();
}

// ---- RAW frames -----------------------------------------------------------------------------

/// One synthetic DNG of the RAW test camera, written to a fresh folder.
fn dng() -> (PathBuf, PathBuf) {
    let directory = temp_dir("panorama-frames-raw");
    let bracket = hdr_bracket::Bracket::new(&hdr_bracket::BracketSpec {
        frames: 2,
        noise: false,
        ..hdr_bracket::BracketSpec::default()
    });
    luxforge_raw::admit_test_cameras();
    let path = directory.join("LXF_0001.DNG");
    fs::write(&path, bracket.dng(0)).unwrap();
    (directory, path)
}

fn committed_index() -> LensIndex {
    LensIndex::parse(
        &fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/lensfun/index.json"
        ))
        .unwrap(),
    )
    .unwrap()
}

/// The optics of a Nikon Z 6 with the NIKKOR Z 24-70mm f/4 S at 35 mm whose source does not
/// correct distortion, as its RAW reports them.
fn nikon_optics(distortion: OpticalStatus) -> SourceOptics {
    let entry = |status| OpticalEntry {
        status,
        provenance: "test".into(),
    };
    SourceOptics {
        identity: OpticalIdentity {
            make: Some("NIKON CORPORATION".into()),
            model: Some("NIKON Z 6".into()),
            lens_make: None,
            lens_model: Some("NIKKOR Z 24-70mm f/4 S".into()),
            focal_mm: Some(35.0),
            focal_35mm: Some(35),
        },
        ledger: OpticalLedger {
            distortion: entry(distortion),
            lateral_ca: entry(OpticalStatus::KnownUnapplied),
            shading: entry(OpticalStatus::KnownUnapplied),
            interpretation: "raw-mosaic:test".into(),
        },
    }
}

#[test]
#[slow]
fn panorama_frames_raw_without_a_profile_is_the_shared_development_exactly() {
    let (directory, path) = dng();
    let before = sha256(&path);
    lens::index::set_for_test(Ok(Arc::new(committed_index())));
    // An editor's development held on the source worker's gate throughout: the frame never waits
    // for it, and takes no lease of its own.
    let gate = Arc::new(PlaneGate::default());
    let held = gate.lease();
    let control = JobControl::new();
    let mut accounting = accounting();
    let spool = directory.join("input-0.spool");
    let frame = read_frame(
        &SharedFrames,
        &FrameRequest {
            path: &path,
            gains: None,
            lens: LensChoice::Detect,
            exposure_scale: 1.0,
            colour: false,
        },
        &spool,
        &control,
        &mut accounting,
    )
    .unwrap();
    lens::index::clear_for_test();
    assert_eq!(gate.counted(), 1);
    drop(held);
    assert_eq!(gate.counted(), 0);
    // The synthetic body is not in the lens database: nothing is detected, nothing baked.
    assert_eq!(
        frame.optics.ledger.distortion.status,
        OpticalStatus::KnownUnapplied
    );
    assert!(frame.lens.is_none());
    let gains = frame.gains.unwrap();
    // The shared development at the same gains, developed again on its own.
    let shared = SharedDevelopment
        .develop(&path, Some(gains), &control)
        .unwrap()
        .development
        .unwrap();
    let (width, height) = (shared.image.width(), shared.image.height());
    assert_eq!((frame.spooled.width, frame.spooled.height), (width, height));
    assert_eq!(accounting.development_bytes, shared.image.plane_bytes());
    let pixels = spooled(&spool);
    assert_eq!(pixels.len(), (width * height) as usize);
    let reader = shared.image.reader();
    let mut clipped = 0;
    for y in 0..height {
        for x in 0..width {
            let want = reader.pixel(x, y).unwrap();
            let (rgb, level) = pixels[(y * width + x) as usize];
            assert_eq!(rgb.map(f32::to_bits), want.map(f32::to_bits), "({x}, {y})");
            assert_eq!(level, shared.clip.level(want));
            clipped += u64::from(level.is_clipped());
        }
    }
    assert!(clipped > 0, "the synthetic sky clips");
    assert_eq!(frame.spooled.clipped, clipped);
    // At a quarter of the exposure every sample is a quarter, exactly, and the clipping evidence
    // is the development's.
    let FramePixels::Raw(development) = SharedFrames
        .develop(&path, Some(gains), &control)
        .unwrap()
        .pixels
    else {
        panic!("a RAW frame");
    };
    let quarter = directory.join("input-1.spool");
    spool_frame(
        &quarter,
        &FramePixels::Raw(development),
        &FrameOptions {
            lens: None,
            exposure_scale: 0.25,
            colour: false,
        },
        &control,
        &mut accounting,
    )
    .unwrap();
    for (scaled, full) in spooled(&quarter).into_iter().zip(&pixels) {
        assert_eq!(scaled.0, full.0.map(|value| value * 0.25));
        assert_eq!(scaled.1, full.1);
    }
    assert_eq!(sha256(&path), before, "the original is byte-identical");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[slow]
fn panorama_frames_raw_with_a_detected_profile_is_the_reference_lens_warp() {
    let (directory, path) = dng();
    let before = sha256(&path);
    let control = JobControl::new();
    let DevelopedFrame { pixels, .. } = SharedFrames.develop(&path, None, &control).unwrap();
    let FramePixels::Raw(development) = &pixels else {
        panic!("a RAW frame");
    };
    let (width, height) = (development.image.width(), development.image.height());
    let index = committed_index();
    // First-open detection's rule: a source known to correct distortion, or not known not to,
    // selects nothing.
    for status in [OpticalStatus::Applied, OpticalStatus::Unknown] {
        assert!(
            LensBake::detect(&index, &nikon_optics(status), width, height)
                .unwrap()
                .is_none()
        );
    }
    let bake = LensBake::detect(
        &index,
        &nikon_optics(OpticalStatus::KnownUnapplied),
        width,
        height,
    )
    .unwrap()
    .expect("the Z 6's 24-70 profile is detected");
    let record = bake.record();
    assert_eq!(record.key, bake.key());
    assert!(record.key.starts_with("lf1-"));
    assert_eq!(record.lens_model, "NIKKOR Z 24-70mm f/4 S");
    assert_eq!(record.model, RadialModel::PtLens);
    assert_eq!((record.width, record.height), (width, height));
    assert_eq!(
        record.ledger_entry(),
        OpticalEntry {
            status: OpticalStatus::Applied,
            provenance: format!("panorama-lens-profile:{}", record.key),
        }
    );
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(
        serde_json::from_value::<BakedLensProfile>(json).unwrap(),
        record
    );

    let spool = directory.join("input-0.spool");
    let mut accounting = accounting();
    spool_frame(
        &spool,
        &pixels,
        &FrameOptions {
            lens: Some(&bake),
            exposure_scale: 1.0,
            colour: false,
        },
        &control,
        &mut accounting,
    )
    .unwrap();

    // The reference crate's warp, from the profile's calibration and normalization recomputed
    // independently: Lensfun's interpolation at 35 mm and its normalization from the crop factors
    // and aspect ratio.
    let profile = bake.profile();
    let terms = reference::lensfun_interpolate(&reference::NIKON_Z_24_70, 35.0).unwrap();
    for (got, want) in profile.terms.iter().zip(terms) {
        assert!((got - want).abs() < 1e-12);
    }
    let ns = reference::lensfun_norm(
        width,
        height,
        profile.lens.crop_factor,
        profile.camera.crop_factor,
        profile.lens.aspect_ratio,
    );
    let model = reference::Model::PtLens;
    let cover = reference::cover_scale_lens(model, terms, width, height, ns);
    assert!(cover > 1.0, "the profile's barrel needs a zoom to cover");
    assert!((record.cover / cover - 1.0).abs() < 1e-9);
    let reader = development.image.reader();
    let frame: Vec<[f64; 3]> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| reader.pixel(x, y).unwrap().map(f64::from))
        .collect();
    let centre = [f64::from(width) / 2.0, f64::from(height) / 2.0];
    let pixels = spooled(&spool);
    let mut worst = 0.0_f64;
    let mut moved = 0;
    for y in 0..height {
        for x in 0..width {
            let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            let q = reference::radial_input(p, centre, ns, model, terms, cover);
            let want = reference::bilinear_linear(&frame, width, height, q);
            let (rgb, level) = pixels[(y * width + x) as usize];
            for c in 0..3 {
                worst = worst.max((f64::from(rgb[c]) - want[c]).abs() / want[c].abs().max(1.0));
            }
            moved += usize::from(rgb != reader.pixel(x, y).unwrap());
            // The evidence is never below the nearest tap's.
            let nearest = development.clip.level(
                reader
                    .pixel(
                        (q[0].floor().max(0.0) as u32).min(width - 1),
                        (q[1].floor().max(0.0) as u32).min(height - 1),
                    )
                    .unwrap(),
            );
            assert!(level >= nearest, "({x}, {y})");
        }
    }
    assert!(worst <= WARP_TOLERANCE, "{worst:e}");
    assert!(
        moved > (width * height) as usize / 2,
        "the profile moves the frame"
    );
    assert_eq!(sha256(&path), before, "the original is byte-identical");
    fs::remove_dir_all(directory).unwrap();
}

// ---- Bounds and cancellation ----------------------------------------------------------------

/// A synthetic RAW camera whose every development's planes it watches: each development must be
/// gone before the next is made.
struct Watching {
    developed: Mutex<Vec<Weak<Vec<f32>>>>,
}

impl FrameDeveloper for Watching {
    fn develop(
        &self,
        path: &Path,
        gains: Option<[f32; 3]>,
        _control: &JobControl,
    ) -> Result<DevelopedFrame, Error> {
        let mut developed = self.developed.lock().unwrap();
        assert!(
            developed.iter().all(|planes| planes.upgrade().is_none()),
            "a development is made while another is held"
        );
        let (width, height) = (96_u32, 64_u32);
        let n = (width * height) as usize;
        let planes: Arc<Vec<f32>> = Arc::new(
            (0..3 * n)
                .map(|i| (i % 97) as f32 / 97.0 + developed.len() as f32 * 0.01)
                .collect(),
        );
        developed.push(Arc::downgrade(&planes));
        let header = HeaderMetadata {
            camera: Some(CameraBody {
                make: "NIKON CORPORATION".into(),
                model: "NIKON Z 6".into(),
                serial: None,
            }),
            exposure: Exposure {
                focal_mm: Some(35.0),
                ..Exposure::default()
            },
            ..HeaderMetadata::default()
        };
        let gains = gains.unwrap_or([2.0, 1.0, 1.5]);
        Ok(DevelopedFrame {
            developed: Developed {
                path: path.to_path_buf(),
                fingerprint: sha256(path),
                frame: FrameMetadata {
                    identity: sha256(path),
                    make: Some("NIKON CORPORATION".into()),
                    model: Some("NIKON Z 6".into()),
                    recording: Recording::Raw {
                        mode: "nikon-z6-14bit-lossless".into(),
                        layout: SensorLayout::Bayer,
                    },
                    width,
                    height,
                    orientation: 1,
                    exposure: settings(&header),
                    captured_at_ms: None,
                },
                header,
                place: None,
                volume: crate::index::volume_of(path, crate::editor::now_ms())?,
                development: None,
            },
            optics: nikon_optics(OpticalStatus::KnownUnapplied),
            pixels: FramePixels::Raw(Development {
                gains,
                cam_xyz: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]],
                image: LinearImage::new(width, height, planes)?,
                clip: SensorClipModel::new(
                    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    gains,
                    SensorLayout::Bayer,
                )?,
            }),
        })
    }
}

#[test]
fn panorama_frames_hold_one_development_at_a_time_and_stop_when_cancelled() {
    let directory = temp_dir("panorama-frames-bounds");
    let paths: Vec<PathBuf> = (0..3)
        .map(|at| {
            let path = directory.join(format!("frame-{at}.raw"));
            fs::write(&path, [at as u8; 16]).unwrap();
            path
        })
        .collect();
    let developer = Watching {
        developed: Mutex::new(Vec::new()),
    };
    let control = JobControl::new();
    let mut accounting = accounting();
    // The reference first, at its own gains and with no profile; the rest at its gains.
    let mut gains = None;
    for (at, path) in paths.iter().enumerate() {
        let frame = read_frame(
            &developer,
            &FrameRequest {
                path,
                gains,
                lens: LensChoice::Use(None),
                exposure_scale: 1.0,
                colour: at == 0,
            },
            &directory.join(format!("input-{at}.spool")),
            &control,
            &mut accounting,
        )
        .unwrap();
        gains.get_or_insert(frame.gains.unwrap());
        assert_eq!(frame.gains, Some([2.0, 1.0, 1.5]));
        assert_eq!(frame.cam_xyz.unwrap()[3], [0.0; 3]);
        assert_eq!(frame.spooled.colour.is_some(), at == 0);
        assert_eq!(frame.spooled.proxy.frame_dimensions(), (96, 64));
    }
    assert_eq!(developer.developed.lock().unwrap().len(), 3);
    assert_eq!(accounting.scratch_bytes, 0);
    assert_eq!(accounting.scratch_peak_bytes, band_bytes(96));
    assert_eq!(accounting.temp_bytes, 3 * 96 * 64 * SPOOL_PIXEL);
    assert_eq!(accounting.development_bytes, 3 * 96 * 64 * 4);

    // A band past the scratch limit is refused before anything is written.
    let pixels = developer.develop(&paths[0], None, &control).unwrap().pixels;
    let refused = directory.join("refused.spool");
    let mut tight = Accounting {
        scratch_limit_bytes: band_bytes(96) - 1,
        ..Accounting::default()
    };
    let options = FrameOptions {
        lens: None,
        exposure_scale: 1.0,
        colour: false,
    };
    let error = spool_frame(&refused, &pixels, &options, &control, &mut tight).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(!refused.exists());
    // An exposure scale is finite and positive.
    for scale in [0.0, f64::NAN] {
        let error = spool_frame(
            &refused,
            &pixels,
            &FrameOptions {
                exposure_scale: scale,
                ..options
            },
            &control,
            &mut accounting,
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
    }
    // Cancelled, the spooling stops at its next band and its partial file is removed.
    let cancelled = directory.join("cancelled.spool");
    control.cancel("test");
    let error = spool_frame(&cancelled, &pixels, &options, &control, &mut accounting).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(!cancelled.exists());
    assert_eq!(accounting.scratch_bytes, 0);
    drop(pixels);
    let error = read_frame(
        &developer,
        &FrameRequest {
            path: &paths[0],
            gains: None,
            lens: LensChoice::Use(None),
            exposure_scale: 1.0,
            colour: false,
        },
        &cancelled,
        &control,
        &mut accounting,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(
        developer.developed.lock().unwrap().len(),
        4,
        "a cancelled job develops nothing more"
    );
    fs::remove_dir_all(directory).unwrap();
}
