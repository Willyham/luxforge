//! A panorama's working pixels (`docs/design/panorama-merge.md`, "Working pixels"): each frame read
//! once, developed, and streamed by bands into the job's spool and its analysis proxy, as an HDR
//! input is (`crate::merge::job`), off the catalog owner and outside the source worker's plane
//! gate, so a frame never waits on a development the editor holds, nor the editor on it.
//!
//! - **RAW frames** develop through the shared verified development
//!   ([`crate::merge::job::develop_read`]) at the merge's white balance (the reference frame's
//!   as-shot gains, which the reference, developed first, answers), with their required embedded
//!   corrections (DNG opcodes), into float32 linear sRGB/D65. When the lens module's first-open
//!   detection would select a profile for the reference frame ([`LensBake::detect`]), that
//!   profile's distortion mapping is applied to every frame as the reference renderer applies a
//!   lens layer: each output pixel centre mapped through the profile's [`WarpStep`] and sampled
//!   bilinearly with clamped taps ([`spool_frame`]). Otherwise a frame is the shared development
//!   exactly.
//! - **JPEG frames** decode through the shared JPEG decode and its limits
//!   ([`crate::source::decode_source_with`]) with their embedded ICC profile honoured
//!   ([`RgbProfile`]; sRGB, Display P3 and any other RGB matrix/TRC profile, sRGB when none), into
//!   float32 linear sRGB/D65 keeping negative and above-one components. No lens profile applies.
//! - **Exposure.** Each frame's samples may be multiplied by its exposure scale (`2^−offset_ev`,
//!   [`crate::merge::hdr::ResolvedExposure::scale`]); a scale of exactly 1 leaves the values exact.
//! - **Clipping evidence** is each spooled pixel's [`SensorLevel`], from the samples before the
//!   exposure scale. A RAW pixel's is its sensor-clipping level ([`SensorClipModel`]); through a
//!   lens profile, the largest level among the taps the sample reads. A JPEG pixel is
//!   [`SensorLevel::CLIPPED`] exactly where any encoded channel is 255, and otherwise the largest
//!   channel's linear value at its own tone curve ([`RgbProfile::encoded_fraction`]).
//!
//! **The spool** is HDR's format (`crate::merge::job::SPOOL_PIXEL`): row-major upright pixels, each
//! its three little-endian binary32 samples and its little-endian [`SensorLevel`], so the merge
//! job reads a panorama's spool back as it reads a bracket's.
//!
//! **Bounds.** [`read_frame`] holds one frame's pixels from its development to the end of its
//! spooling and drops them before it returns: a job that reads its frames through it holds one
//! development at a time, never N. A RAW development is HDR's (its mosaic and float planes); a
//! JPEG's is its upright RGBA8 frame, within the shared decode's limits (`JPEG_LIMITS`, 64 MP, so
//! at most 256 MiB), and its encoded bytes until the decode ends (at most `MAX_JPEG_BYTES`). Beside
//! it the spooling holds one band's planes, levels and encoding, taken from the job's scratch
//! ([`Accounting`]), and the proxies' accumulators. Spooling is cancellable per band; a frame
//! whose spooling fails or is cancelled removes its partial spool file.
use crate::{
    Error, SourceImage,
    jobs::JobControl,
    library::develop::{JpegProfiles, read_with},
    merge::{
        hdr::{ColourProxy, ColourProxyBuilder, FrameRect, SensorClipModel, SensorLevel},
        job::{Accounting, Developed, Development, SPOOL_PIXEL, develop_read},
        panorama::{PanoramaProxy, ProxyBuilder, ProxyRows},
    },
    modules::{
        Mapping, RadialModel, Stage, WarpStep,
        lens::{self, index::LensIndex, payload::Profile},
    },
    profile::RgbProfile,
    render::map::WarpChain,
    source::{OpticalIdentity, SourceOptics, ViewReader},
};
use luxforge_raw::{OpticalEntry, OpticalStatus};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    sync::Arc,
};

/// Rows a frame is spooled and reduced in at a time, as an HDR input is.
const BAND_ROWS: u32 = 16;

/// One frame's pixels, held while it is spooled and then dropped.
pub(crate) enum FramePixels {
    /// A RAW frame's shared development at the merge's white balance.
    Raw(Development),
    /// A JPEG frame's upright decode and how its codes read as linear sRGB/D65.
    Jpeg {
        image: SourceImage,
        colour: Box<RgbProfile>,
    },
}

impl FramePixels {
    /// The upright frame's width and height.
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Raw(development) => (development.image.width(), development.image.height()),
            Self::Jpeg { image, .. } => (image.width, image.height),
        }
    }

    /// The bytes the frame's pixels hold: a development's float planes, or a JPEG's RGBA8 frame.
    pub(crate) fn bytes(&self) -> u64 {
        match self {
            Self::Raw(development) => development.image.plane_bytes(),
            Self::Jpeg { image, .. } => image.rgba.len() as u64,
        }
    }
}

/// One frame read once and developed: what it is, its optical identity and correction ledger, and
/// its pixels.
pub(crate) struct DevelopedFrame {
    /// Its path, fingerprint, header, place, volume and frame metadata, as an HDR input's
    /// development reports them. Its `development` is always `None`: the pixels are
    /// [`Self::pixels`].
    pub developed: Developed,
    /// Its camera and lens identity, with what its source corrects: a RAW's from its capture
    /// metadata and its decoder's ledger, a JPEG's unknown.
    pub optics: SourceOptics,
    pub pixels: FramePixels,
}

/// Reads and develops one panorama frame. The merge's is [`SharedFrames`]; a test may supply a
/// synthetic camera.
pub(crate) trait FrameDeveloper: Send + Sync {
    /// Read the file at `path` once and develop it: a RAW at `gains`, or at its own as-shot gains
    /// when none are named; a JPEG decoded with its colour profile.
    fn develop(
        &self,
        path: &Path,
        gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<DevelopedFrame, Error>;
}

/// The shared verified read and development: one bounded read that streams the file's SHA-256 with
/// its signature held throughout, a JPEG's colour profile held to the merge's rule
/// ([`JpegProfiles::Rgb`]); a RAW through the shared development ([`develop_read`]); a JPEG through
/// the shared decode with its profile read ([`crate::source::decode_source_with`]).
pub(crate) struct SharedFrames;

impl FrameDeveloper for SharedFrames {
    fn develop(
        &self,
        path: &Path,
        gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<DevelopedFrame, Error> {
        use crate::editor::ReadContent;
        let mut read = read_with(path, true, JpegProfiles::Rgb, control, &|_| {})?;
        let (optics, jpeg) = match &read.kept {
            Some(ReadContent::Raw { sensor, capture }) => (
                Some(SourceOptics {
                    identity: OpticalIdentity::from(capture.as_ref()),
                    ledger: luxforge_raw::optical_ledger(sensor.metadata()),
                }),
                None,
            ),
            Some(ReadContent::Jpeg(bytes)) => (None, Some(Arc::clone(bytes))),
            None => (None, None),
        };
        if jpeg.is_some() {
            // The bytes are decoded below, not by the development, which reports a JPEG's
            // identity from its header alone.
            read.kept = None;
        }
        let mut developed = develop_read(read, gains, control)?;
        let (optics, pixels) = match (developed.development.take(), optics, jpeg) {
            (Some(development), Some(optics), None) => (optics, FramePixels::Raw(development)),
            (None, None, Some(bytes)) => {
                control.checkpoint()?;
                let (image, colour) = crate::source::decode_source_with(
                    &bytes,
                    developed.fingerprint.clone(),
                    |profile, components| RgbProfile::of(profile, components).map(Box::new),
                )?;
                drop(bytes);
                control.checkpoint()?;
                if (image.width, image.height) != (developed.frame.width, developed.frame.height) {
                    return Err(Error::internal(
                        "a JPEG frame decoded at another size than its header read",
                    ));
                }
                (
                    SourceOptics::jpeg(&image.capture),
                    FramePixels::Jpeg { image, colour },
                )
            }
            _ => {
                return Err(Error::internal(
                    "a panorama frame was read without its pixels",
                ));
            }
        };
        Ok(DevelopedFrame {
            developed,
            optics,
            pixels,
        })
    }
}

/// The lens profile a panorama bakes into every RAW frame: the profile the lens module's
/// first-open detection selects for the reference frame, resolved and frozen as its selection is,
/// and the distortion mapping it compiles to at the frame's upright size.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LensBake {
    profile: Profile,
    step: WarpStep,
    mapping: Mapping,
    width: u32,
    height: u32,
}

/// What a panorama records of the lens profile it baked: in its provenance, and in its result's
/// optics ledger as an applied distortion correction ([`Self::ledger_entry`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakedLensProfile {
    /// The profile's key in the pinned Lensfun index, as the lens module's selection records it.
    pub key: String,
    pub lens_maker: String,
    pub lens_model: String,
    pub focal_mm: f64,
    pub model: RadialModel,
    pub terms: [f64; 3],
    /// The profile's normalization at the frames' size.
    pub unit_scale: f64,
    /// The scale that keeps the corrected frame covering its canvas, as the lens layer's.
    pub cover: f64,
    /// The upright frame size the profile was resolved for and applied at.
    pub width: u32,
    pub height: u32,
}

impl BakedLensProfile {
    /// The derived source's distortion entry: applied, naming this profile.
    pub fn ledger_entry(&self) -> OpticalEntry {
        OpticalEntry {
            status: OpticalStatus::Applied,
            provenance: format!("panorama-lens-profile:{}", self.key),
        }
    }
}

impl LensBake {
    /// The profile first-open detection would select for a reference frame with `optics`
    /// developed upright at `width × height` ([`lens::first_open_profile`]: its source known not
    /// to correct distortion and an eligible detected profile), resolved and frozen as selecting it
    /// does; none when detection selects nothing.
    pub(crate) fn detect(
        index: &LensIndex,
        optics: &SourceOptics,
        width: u32,
        height: u32,
    ) -> Result<Option<Self>, Error> {
        let stage = Stage { width, height };
        let Some(key) = lens::first_open_profile(index, optics, stage) else {
            return Ok(None);
        };
        let profile = lens::frozen_profile(index, optics, stage, &key)?;
        let step = lens::warp_step(&profile, stage)?;
        Ok(Some(Self {
            mapping: Mapping::Warp(Arc::new(WarpChain::new(step))),
            profile,
            step,
            width,
            height,
        }))
    }

    /// [`Self::detect`] against the shared lens profile index, waiting for its one background
    /// parse. For the merge lane's worker, never the catalog owner or a pool thread. An index that
    /// could not be loaded is its `not-ready` refusal: whether a profile applies is unknown.
    pub(crate) fn detect_shared(
        optics: &SourceOptics,
        width: u32,
        height: u32,
    ) -> Result<Option<Self>, Error> {
        lens::index::wait_ready();
        let index = lens::index::shared()?;
        Self::detect(&index, optics, width, height)
    }

    /// The profile's key.
    pub(crate) fn key(&self) -> &str {
        &self.profile.key
    }

    /// The frame size the profile applies at.
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// What a panorama records of it.
    pub(crate) fn record(&self) -> BakedLensProfile {
        BakedLensProfile {
            key: self.profile.key.clone(),
            lens_maker: self.profile.lens.maker.clone(),
            lens_model: self.profile.lens.model.clone(),
            focal_mm: self.profile.focal.mm,
            model: match self.profile.model {
                lens::index::DistortionModel::Poly3 => RadialModel::Poly3,
                lens::index::DistortionModel::Poly5 => RadialModel::Poly5,
                lens::index::DistortionModel::PtLens => RadialModel::PtLens,
            },
            terms: self.profile.terms,
            unit_scale: self.profile.normalization.unit_scale,
            cover: self.step.cover(),
            width: self.width,
            height: self.height,
        }
    }

    /// The frozen profile, for a test that checks the mapping against an independent reference.
    #[cfg(test)]
    pub(crate) fn profile(&self) -> &Profile {
        &self.profile
    }
}

/// How one frame is spooled.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameOptions<'a> {
    /// The lens profile baked into a RAW frame; a JPEG takes none.
    pub lens: Option<&'a LensBake>,
    /// The factor bringing the frame's samples to the reference's brightness, `2^−offset_ev`
    /// ([`crate::merge::hdr::ResolvedExposure::scale`]); exactly 1 leaves them exact.
    pub exposure_scale: f64,
    /// Build the merge preview's [`ColourProxy`] beside the registration proxy.
    pub colour: bool,
}

/// What spooling one frame leaves: its registration proxy ([`PanoramaProxy`], unusable where a
/// box holds a clipped sample), its colour proxy, the bytes written and its clipping count.
#[derive(Debug)]
pub(crate) struct SpooledFrame {
    pub width: u32,
    pub height: u32,
    pub proxy: PanoramaProxy,
    /// With [`FrameOptions::colour`].
    pub colour: Option<ColourProxy>,
    /// The bytes written to the spool file: [`SPOOL_PIXEL`] a pixel.
    pub bytes: u64,
    /// The spooled pixels marked [`SensorLevel::CLIPPED`].
    pub clipped: u64,
}

/// Every byte one band of a `width`-pixel-wide frame holds while it is spooled: its planes, levels
/// and encoding.
pub(crate) fn band_bytes(width: u32) -> u64 {
    u64::from(width) * u64::from(BAND_ROWS) * (12 + 2 + SPOOL_PIXEL)
}

/// Write `pixels`' upright rows to the spool file at `path` ([`SPOOL_PIXEL`] bytes a pixel), a
/// band at a time, reducing each band into the frame's analysis proxy (and, with `colour`, its
/// colour proxy) as it goes. A RAW frame is the development, or its lens profile's mapping of it;
/// a JPEG frame its codes as linear sRGB/D65. The band's bytes are taken from `accounting`'s
/// scratch for the spooling and the bytes written are added to its temporary bytes. Cancellable
/// per band; on any failure the partial file is removed.
pub(crate) fn spool_frame(
    path: &Path,
    pixels: &FramePixels,
    options: &FrameOptions<'_>,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<SpooledFrame, Error> {
    let (width, height) = pixels.dimensions();
    let scale = options.exposure_scale;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(Error::validation(
            "a frame's exposure scale must be finite and positive",
        ));
    }
    if let Some(lens) = options.lens {
        if matches!(pixels, FramePixels::Jpeg { .. }) {
            return Err(Error::internal("a JPEG frame takes no lens profile"));
        }
        if lens.dimensions() != (width, height) {
            return Err(Error::validation(format!(
                "the lens profile was resolved for {} × {} frames, not {width} × {height}",
                lens.width, lens.height
            )));
        }
    }
    let band = band_bytes(width);
    accounting.take(band, "a frame band")?;
    let spooled = write_spool(path, pixels, options, control, accounting);
    accounting.give(band);
    if spooled.is_err() {
        let _ = fs::remove_file(path);
    }
    spooled
}

fn write_spool(
    path: &Path,
    pixels: &FramePixels,
    options: &FrameOptions<'_>,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<SpooledFrame, Error> {
    let (width, height) = pixels.dimensions();
    let mut builder = ProxyBuilder::new(width, height)?;
    let cancel = control.render_cancel();
    let mut colour = options
        .colour
        .then(|| ColourProxyBuilder::new(width, height))
        .transpose()?;
    let w = width as usize;
    let rows = BAND_ROWS as usize;
    let mut planes = [
        vec![0.0_f32; w * rows],
        vec![0.0_f32; w * rows],
        vec![0.0_f32; w * rows],
    ];
    let mut levels = vec![SensorLevel(0); w * rows];
    let mut flags = vec![false; w * rows];
    let mut bytes = Vec::with_capacity(w * rows * SPOOL_PIXEL as usize);
    let mut file = File::create(path).map_err(|error| spool_error(error.kind()))?;
    let mut written = 0_u64;
    let mut clipped = 0_u64;
    let scale = options.exposure_scale;
    for top in (0..height).step_by(rows) {
        control.checkpoint()?;
        let band = (height - top).min(BAND_ROWS) as usize;
        let mut put = |i: usize, rgb: [f32; 3], level: SensorLevel| {
            for (plane, value) in planes.iter_mut().zip(rgb) {
                plane[i] = scaled(value, scale);
            }
            levels[i] = level;
        };
        match (pixels, options.lens) {
            (FramePixels::Raw(development), None) => {
                let reader = development.image.reader();
                let walk = reader
                    .walk((0, top), (1, 0), (0, 1), width, band as u32)
                    .ok_or_else(|| Error::internal("a development band lies outside its view"))?;
                reader.visit(walk, |i, rgb| {
                    put(i, rgb, development.clip.level(rgb));
                    Ok::<(), Error>(())
                })?;
            }
            (FramePixels::Raw(development), Some(lens)) => {
                let reader = development.image.reader();
                for row in 0..band {
                    let y = top + row as u32;
                    for x in 0..width {
                        let (rgb, level) = warped(&reader, &development.clip, &lens.mapping, x, y)?;
                        put(row * w + x as usize, rgb, level);
                    }
                }
            }
            (FramePixels::Jpeg { image, colour }, _) => {
                let start = top as usize * w * 4;
                let rgba = &image.rgba[start..start + band * w * 4];
                for (i, pixel) in rgba.chunks_exact(4).enumerate() {
                    let codes = [pixel[0], pixel[1], pixel[2]];
                    put(i, colour.linear_srgb(codes), jpeg_level(colour, codes));
                }
            }
        }
        bytes.clear();
        for i in 0..band * w {
            for plane in &planes {
                bytes.extend_from_slice(&plane[i].to_le_bytes());
            }
            bytes.extend_from_slice(&levels[i].0.to_le_bytes());
            flags[i] = levels[i].is_clipped();
            clipped += u64::from(flags[i]);
        }
        let rect = FrameRect {
            width: w,
            height: band,
            stride: w,
            rgb: [&planes[0], &planes[1], &planes[2]],
            levels: &levels,
        };
        builder.push_rows(
            &ProxyRows {
                width: w,
                height: band,
                stride: w,
                rgb: rect.rgb,
                clipped: Some(&flags),
            },
            cancel,
        )?;
        if let Some(colour) = &mut colour {
            colour.push_rows(&rect)?;
        }
        file.write_all(&bytes)
            .map_err(|error| spool_error(error.kind()))?;
        written += bytes.len() as u64;
        accounting.temp_bytes += bytes.len() as u64;
    }
    file.flush().map_err(|error| spool_error(error.kind()))?;
    Ok(SpooledFrame {
        width,
        height,
        proxy: builder.finish()?,
        colour: colour.map(ColourProxyBuilder::finish).transpose()?,
        bytes: written,
        clipped,
    })
}

/// A sample at the frame's exposure scale; exact at 1.
#[inline]
fn scaled(value: f32, scale: f64) -> f32 {
    if scale == 1.0 {
        value
    } else {
        (f64::from(value) * scale) as f32
    }
}

/// A JPEG pixel's clipping evidence: clipped where any encoded channel is 255, otherwise its
/// largest channel's linear value at its own tone curve, which stays below clipped.
#[inline]
fn jpeg_level(colour: &RgbProfile, codes: [u8; 3]) -> SensorLevel {
    if codes.contains(&u8::MAX) {
        return SensorLevel::CLIPPED;
    }
    let fraction = colour.encoded_fraction(codes).clamp(0.0, 1.0);
    SensorLevel((fraction * SensorLevel::SCALE) as u16)
}

/// Output pixel `(x, y)` of a development through a lens profile's mapping, as the reference
/// renderer's linear resample draws it: the pixel centre mapped into the development, the four
/// taps around it clamped to the frame and blended bilinearly in f64. Its level is the largest
/// among the taps the blend weighs.
#[inline]
fn warped(
    reader: &ViewReader<'_>,
    clip: &SensorClipModel,
    mapping: &Mapping,
    x: u32,
    y: u32,
) -> Result<([f32; 3], SensorLevel), Error> {
    let (width, height) = reader.dimensions();
    let (u, v) = mapping.input_at(f64::from(x) + 0.5, f64::from(y) + 0.5);
    if !u.is_finite() || !v.is_finite() {
        return Err(Error::render(
            "a lens profile mapped a frame pixel to no point",
        ));
    }
    let (sx, sy) = (u - 0.5, v - 0.5);
    let (left, top) = (sx.floor(), sy.floor());
    let (wx, wy) = (sx - left, sy - top);
    let (x0, x1) = (clamp_index(left, width), clamp_index(left + 1.0, width));
    let (y0, y1) = (clamp_index(top, height), clamp_index(top + 1.0, height));
    let taps = [
        ((x0, y0), (1.0 - wx) * (1.0 - wy)),
        ((x1, y0), wx * (1.0 - wy)),
        ((x0, y1), (1.0 - wx) * wy),
        ((x1, y1), wx * wy),
    ];
    let mut corners = [([0.0_f32; 3], 0.0_f64); 4];
    let mut level = SensorLevel(0);
    for (corner, ((tx, ty), weight)) in corners.iter_mut().zip(taps) {
        let rgb = reader
            .pixel(tx, ty)
            .ok_or_else(|| Error::internal("a lens tap lies outside its frame"))?;
        if weight > 0.0 {
            level = level.max(clip.level(rgb));
        }
        *corner = (rgb, weight);
    }
    let rgb = std::array::from_fn(|channel| {
        corners
            .iter()
            .map(|(pixel, weight)| f64::from(pixel[channel]) * weight)
            .sum::<f64>() as f32
    });
    Ok((rgb, level))
}

/// The tap index of `value`, clamped to `[0, limit − 1]` as the reference renderer clamps it.
#[inline]
fn clamp_index(value: f64, limit: u32) -> u32 {
    let last = limit.saturating_sub(1);
    if value <= 0.0 {
        0
    } else if value >= f64::from(last) {
        last
    } else {
        value as u32
    }
}

fn spool_error(kind: std::io::ErrorKind) -> Error {
    crate::atomic_file::file_error("cannot write the merge's working file", kind)
}

/// Whether a frame takes the reference frame's lens profile or detects its own: the reference,
/// developed first, detects; every other frame takes what it chose.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LensChoice<'a> {
    /// Detect from this frame's own optics ([`LensBake::detect_shared`]), as a RAW reference
    /// frame does. A JPEG detects nothing.
    Detect,
    /// Apply what the reference frame's detection chose, or no profile: also the reference's own
    /// choice when the person has turned off lens correction for new RAW photos.
    Use(Option<&'a LensBake>),
}

/// One frame to read.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameRequest<'a> {
    pub path: &'a Path,
    /// The white balance a RAW frame develops at: the reference's, or `None` for the reference
    /// itself, developed at its own as-shot gains.
    pub gains: Option<[f32; 3]>,
    pub lens: LensChoice<'a>,
    pub exposure_scale: f64,
    pub colour: bool,
}

/// What reading one frame leaves once its pixels are dropped.
pub(crate) struct ReadFrame {
    /// What it is; its `development` is `None`.
    pub developed: Developed,
    pub optics: SourceOptics,
    /// The white balance a RAW frame was developed at, and its camera matrix.
    pub gains: Option<[f32; 3]>,
    pub cam_xyz: Option<[[f32; 3]; 4]>,
    /// The lens profile baked into it: for a reference that detected one, the profile every other
    /// frame takes.
    pub lens: Option<LensBake>,
    pub spooled: SpooledFrame,
}

/// Read, develop and spool one frame to `spool`, then drop its pixels: a job that reads its frames
/// through this holds one development at a time. The development's bytes are recorded as
/// `accounting`'s largest development. Detection, for [`LensChoice::Detect`], reads the shared lens
/// profile index.
pub(crate) fn read_frame(
    developer: &dyn FrameDeveloper,
    request: &FrameRequest<'_>,
    spool: &Path,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<ReadFrame, Error> {
    control.checkpoint()?;
    let DevelopedFrame {
        developed,
        optics,
        pixels,
    } = developer.develop(request.path, request.gains, control)?;
    accounting.development_bytes = accounting.development_bytes.max(pixels.bytes());
    let (width, height) = pixels.dimensions();
    let (gains, cam_xyz) = match &pixels {
        FramePixels::Raw(development) => (Some(development.gains), Some(development.cam_xyz)),
        FramePixels::Jpeg { .. } => (None, None),
    };
    let lens = match (&pixels, request.lens) {
        (FramePixels::Jpeg { .. }, LensChoice::Detect | LensChoice::Use(None)) => None,
        (FramePixels::Jpeg { .. }, LensChoice::Use(Some(_))) => {
            return Err(Error::internal("a JPEG frame takes no lens profile"));
        }
        (FramePixels::Raw(_), LensChoice::Detect) => {
            LensBake::detect_shared(&optics, width, height)?
        }
        (FramePixels::Raw(_), LensChoice::Use(lens)) => lens.cloned(),
    };
    let spooled = spool_frame(
        spool,
        &pixels,
        &FrameOptions {
            lens: lens.as_ref(),
            exposure_scale: request.exposure_scale,
            colour: request.colour,
        },
        control,
        accounting,
    )?;
    drop(pixels);
    Ok(ReadFrame {
        developed,
        optics,
        gains,
        cam_xyz,
        lens,
        spooled,
    })
}

#[cfg(test)]
#[path = "panorama_frames.rs"]
mod panorama_frames;
