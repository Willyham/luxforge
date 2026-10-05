//! Making a file's preview tiers from what the file carries: every tier a baseline JPEG, upright
//! (the file's EXIF orientation applied, so a client draws it as it is), fitted within its tier's
//! long edge and never enlarged, with the embedded ICC profile kept.
//!
//! - **A JPEG original.** Its thumbnail stage is the EXIF thumbnail the index row locates, read at
//!   its offset and length and nothing else. Its embedded stage, and its loupe tier, is the
//!   original itself, decoded at the DCT scale that covers the fitted tier and box-downscaled.
//! - **A RAW original.** One [`EmbeddedPreviews`] open lists every image the file carries. The
//!   thumbnail stage is the smallest extractable image (a JPEG or an RGB bitmap: most Nikon
//!   thumbnails are 160 × 120 bitmaps) when the file carries another; the grid and loupe tiers come
//!   from the largest JPEG decoded at a scale, then the other JPEGs, then the largest bitmap.
//!   OM System, Olympus and Panasonic files carry one preview only, and every tier comes from it.
//! - **No usable preview.** A RAW whose listed images are none of them extractable, or none of
//!   which decodes (the Canon EOS R5 Mark II's and R8's H.265 previews), is [`Found::Unusable`]
//!   with the reason, and the lane hands it to [`develop_instead`], which develops it neutrally
//!   ([`region::developed_preview`]) for a visible or look-ahead task. A JPEG original that does
//!   not decode is no such case: it fails with its own kind (`invalid-input` for a corrupt one).
//!
//! Many camera JPEGs carry bytes after their last EOI marker (every Canon CR3's full-size preview,
//! the Leica CL's, Q2's and SL2's, some DJI files'), which the strict codec refuses; each is cut
//! at its last EOI here before it is decoded.
//!
//! Everything here is frame work for the lane's workers, never the owner: decoding, resampling
//! and encoding. The decode is one frame at the scale that covers the tier, within the JPEG
//! original's limits (`source::JPEG_LIMITS`); the downscale is the proxy's box downscale and its
//! bands; each step checks the task's cancellation.
use super::{
    FILE_GRID_SIDE,
    region::{self, DevelopedPreview},
};
use crate::{
    Cancel, Error, ErrorKind, PreviewSource, ProxyBounds, ProxyPlan, Raster, SourceImage,
    SourceTag,
    catalog_types::{EmbeddedFormat, FileRecord, FileSignature, HeaderState, PreviewOrigin},
    export::metadata::jpeg_orientation,
    jobs::JobControl,
    source::{JPEG_LIMITS, raw_error, upright_position},
};
use luxforge_jpeg::{Decoder, STRIP_ROWS, Scale, Settings};
use luxforge_raw::{
    EmbeddedImage, EmbeddedPreview, EmbeddedPreviews, MAX_EMBEDDED_IMAGE_BYTES,
    MAX_EMBEDDED_READ_BUDGET, PreviewFormat, RandomAccess, RawError,
};
use std::{borrow::Cow, fs::File, path::Path, sync::Arc};

/// The quality every preview is encoded at.
const QUALITY: u8 = 85;
/// 4:2:0 chroma, as camera previews are stored.
const CHROMA: (u8, u8) = (2, 2);
/// The most bytes read for a JPEG original's recorded thumbnail. EXIF keeps IFD1 inside one APP1
/// segment of at most 64 KiB, so a larger record is not a thumbnail.
const MAX_THUMBNAIL_BYTES: u32 = 64 * 1024;

/// One tier made: its JPEG, its size and what its pixels are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Made {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub origin: PreviewOrigin,
}

/// What making a tier found: the tier, or why a RAW has no usable preview.
#[derive(Debug)]
pub(crate) enum Found {
    Made(Made),
    Unusable(String),
}

/// How a RAW with no usable preview is developed into a tier: the file at a path, known by its
/// signature, developed neutrally and fitted within a long edge, checking a cancel token.
/// [`region::developed_preview`] in production: one development at a time in the process, off
/// the editor's source cache.
pub(crate) type Develop<'a> =
    &'a (dyn Fn(&Path, &FileSignature, u32, &Cancel) -> Result<DevelopedPreview, Error> + Sync);

/// The production [`Develop`].
pub(crate) const DEVELOP: Develop<'static> = &region::developed_preview;

/// The seam for a RAW with no usable preview, which the lane's worker calls for a visible or
/// look-ahead task, with the tier's long edge (`side`) and why the file's own images would not
/// do: the frame developed neutrally by `develop` (one RAW at a time, off the editor's source
/// cache; the development stays kept for the file's 100% regions), fitted within `side` and
/// encoded as the tier, upright, origin [`PreviewOrigin::Developed`]. A camera Luxforge cannot
/// develop fails `unsupported-input` naming why, which the lane remembers for the file's
/// signature; every other failure keeps its own kind.
pub(crate) fn develop_instead(
    record: &FileRecord,
    side: u32,
    why: &str,
    control: &JobControl,
    develop: Develop<'_>,
) -> Result<Made, Error> {
    control.checkpoint()?;
    if record.kind != SourceTag::Raw {
        return Err(Error::internal(format!(
            "{} is a JPEG original, its own preview, and is never developed",
            record.name
        )));
    }
    let developed = develop(
        &record.path,
        &record.signature,
        side,
        control.render_cancel(),
    )
    .map_err(|error| match error.kind {
        ErrorKind::Cancelled => control.cancelled_error(),
        ErrorKind::SourceUnavailable => error,
        kind => Error::new(
            kind,
            format!(
                "{} has no usable preview ({why}) and cannot be developed: {}",
                record.name, error.detail
            ),
        ),
    })?;
    control.checkpoint()?;
    let mut jpeg = Vec::new();
    luxforge_jpeg::encode(
        &mut jpeg,
        developed.width,
        developed.height,
        &developed.rgba,
        &Settings {
            quality: QUALITY,
            chroma: CHROMA,
            segments: &[],
            icc: None,
            pixels_per_inch: None,
        },
        &mut |_| control.checkpoint(),
    )?;
    Ok(Made {
        jpeg,
        width: developed.width,
        height: developed.height,
        origin: PreviewOrigin::Developed,
    })
}

/// The images one file offers one task, opened once: the file, and for a RAW its listing.
pub(crate) enum FileImages {
    Jpeg {
        file: File,
        /// Where the index says the EXIF thumbnail is, and the orientation the header records.
        thumbnail: Option<crate::catalog_types::EmbeddedImage>,
        orientation: u8,
    },
    Raw {
        previews: EmbeddedPreviews<File>,
        /// The listing's images in the order a tier tries them.
        candidates: Vec<EmbeddedPreview>,
        /// The smallest extractable image, when the file carries another.
        thumbnail: Option<EmbeddedPreview>,
        /// Why each listed image the crate does not extract is refused, for a file with none
        /// usable.
        refused: Vec<&'static str>,
    },
}

impl FileImages {
    /// Open `record`'s file. A file that cannot be opened — gone, or on an unmounted volume — is
    /// `source-unavailable`; a RAW LibRaw cannot identify is `invalid-input`.
    pub(crate) fn open(record: &FileRecord, control: &JobControl) -> Result<Self, Error> {
        control.checkpoint()?;
        let file = File::open(&record.path).map_err(|error| unavailable(record, error.kind()))?;
        match record.kind {
            SourceTag::Jpeg => {
                let header = match &record.header {
                    HeaderState::Ok(header) => Some(header),
                    _ => None,
                };
                Ok(Self::Jpeg {
                    file,
                    thumbnail: header.and_then(|header| header.thumbnail),
                    orientation: header
                        .and_then(|header| header.orientation)
                        .map_or(1, |orientation| orientation.get()),
                })
            }
            SourceTag::Raw => {
                let previews =
                    EmbeddedPreviews::open(file, MAX_EMBEDDED_READ_BUDGET, control.flag())
                        .map_err(|error| raw_failure(record, error, control))?;
                let listing = previews.listing();
                let mut candidates: Vec<EmbeddedPreview> = listing
                    .previews
                    .iter()
                    .copied()
                    .filter(|preview| preview.format.extractable())
                    .collect();
                // The largest JPEG first, by stored length (LibRaw lists a CR3's full-size JPEG as
                // 0 × 0), then the other JPEGs, then bitmaps by their pixels.
                candidates.sort_by_key(|preview| {
                    let jpeg = preview.format == PreviewFormat::Jpeg;
                    (
                        !jpeg,
                        std::cmp::Reverse(if jpeg { preview.bytes } else { pixels(preview) }),
                    )
                });
                let thumbnail = candidates
                    .iter()
                    .copied()
                    .min_by_key(|preview| match pixels(preview) {
                        0 => (u64::MAX, preview.bytes),
                        pixels => (pixels, preview.bytes),
                    })
                    .filter(|smallest| {
                        candidates
                            .first()
                            .is_some_and(|largest| largest.index != smallest.index)
                    });
                let refused = listing
                    .previews
                    .iter()
                    .filter(|preview| !preview.format.extractable())
                    .map(|preview| refusal(preview.format))
                    .collect();
                Ok(Self::Raw {
                    previews,
                    candidates,
                    thumbnail,
                    refused,
                })
            }
        }
    }

    /// The thumbnail stage of the grid tier, when the file carries a thumbnail besides the image
    /// its grid tier comes from: fitted within [`FILE_GRID_SIDE`], upright, origin
    /// `exif-thumbnail`. A thumbnail that cannot be read or decoded is no stage, not a failure: the
    /// grid tier still comes from the larger preview. An unreadable file or a cancellation stops
    /// the task.
    pub(crate) fn thumbnail(
        &mut self,
        record: &FileRecord,
        control: &JobControl,
    ) -> Result<Option<Made>, Error> {
        let (pixels, orientation) = match self {
            Self::Jpeg {
                file,
                thumbnail,
                orientation,
            } => {
                let Some(thumbnail) = *thumbnail else {
                    return Ok(None);
                };
                if thumbnail.len() > MAX_THUMBNAIL_BYTES {
                    return Ok(None);
                }
                let mut bytes = vec![0; thumbnail.len() as usize];
                read_exact_at(file, thumbnail.offset(), &mut bytes)
                    .map_err(|error| unavailable(record, error.kind()))?;
                let pixels = match thumbnail.format() {
                    EmbeddedFormat::Jpeg => decode_jpeg(&bytes, FILE_GRID_SIDE, control),
                    EmbeddedFormat::Rgb8 => match thumbnail.size() {
                        Some(size) => rgb_pixels(size.width, size.height, &bytes),
                        None => return Ok(None),
                    },
                };
                (pixels, *orientation)
            }
            Self::Raw {
                previews,
                thumbnail,
                ..
            } => {
                let Some(thumbnail) = *thumbnail else {
                    return Ok(None);
                };
                let orientation = previews.listing().orientation;
                let image = match previews.extract(
                    thumbnail.index,
                    MAX_EMBEDDED_IMAGE_BYTES,
                    control.flag(),
                ) {
                    Ok(image) => image,
                    Err(error) => {
                        return stop_on(raw_failure(record, error, control)).map(|()| None);
                    }
                };
                (embedded_pixels(image, FILE_GRID_SIDE, control), orientation)
            }
        };
        match pixels.and_then(|pixels| {
            finish(
                pixels,
                FILE_GRID_SIDE,
                orientation,
                PreviewOrigin::ExifThumbnail,
                control,
            )
        }) {
            Ok(made) => Ok(Some(made)),
            Err(error) => stop_on(error).map(|()| None),
        }
    }

    /// The file's preview fitted within `side`, upright, origin `embedded`: the JPEG original
    /// itself, or a RAW's largest usable embedded image. [`Found::Unusable`] names why a RAW has
    /// none; a JPEG original that does not decode fails with its own kind, naming the file.
    pub(crate) fn preview(
        &mut self,
        record: &FileRecord,
        side: u32,
        control: &JobControl,
    ) -> Result<Found, Error> {
        match self {
            Self::Jpeg { file, .. } => {
                let bytes = crate::read_bounded_file(file).map_err(|error| {
                    if error.kind == ErrorKind::FileAccess {
                        Error::source_unavailable(format!(
                            "{} cannot be read: {}",
                            record.path.display(),
                            error.detail
                        ))
                    } else {
                        error
                    }
                })?;
                let orientation = jpeg_orientation(&bytes);
                decode_jpeg(&bytes, side, control)
                    .and_then(|pixels| {
                        finish(pixels, side, orientation, PreviewOrigin::Embedded, control)
                    })
                    .map(Found::Made)
                    .map_err(|error| match error.kind {
                        ErrorKind::Cancelled | ErrorKind::SourceUnavailable => error,
                        kind => Error::new(
                            kind,
                            format!("{} does not decode: {}", record.name, error.detail),
                        ),
                    })
            }
            Self::Raw {
                previews,
                candidates,
                refused,
                ..
            } => {
                let orientation = previews.listing().orientation;
                let mut reasons: Vec<String> =
                    refused.iter().map(|why| (*why).to_owned()).collect();
                for candidate in candidates.iter() {
                    let made = match previews.extract(
                        candidate.index,
                        MAX_EMBEDDED_IMAGE_BYTES,
                        control.flag(),
                    ) {
                        Ok(image) => embedded_pixels(image, side, control).and_then(|pixels| {
                            finish(pixels, side, orientation, PreviewOrigin::Embedded, control)
                        }),
                        Err(error) => Err(raw_failure(record, error, control)),
                    };
                    match made {
                        Ok(made) => return Ok(Found::Made(made)),
                        Err(error) => {
                            stop_on(error.clone())?;
                            reasons.push(error.detail);
                        }
                    }
                }
                Ok(Found::Unusable(if reasons.is_empty() {
                    "the file lists no embedded preview".into()
                } else {
                    reasons.join("; ")
                }))
            }
        }
    }
}

/// Stop the task on what reading the file or the task's control says — the file gone or changed,
/// a cancellation — and go on past anything else, which only says this image is not usable.
fn stop_on(error: Error) -> Result<(), Error> {
    match error.kind {
        ErrorKind::SourceUnavailable | ErrorKind::Cancelled => Err(error),
        _ => Ok(()),
    }
}

/// A listed image's pixels as its container declares them; 0 when it declares none.
fn pixels(preview: &EmbeddedPreview) -> u64 {
    u64::from(preview.width) * u64::from(preview.height)
}

/// Why LibRaw's listing holds an image the crate does not extract, as its extraction would say.
fn refusal(format: PreviewFormat) -> &'static str {
    match format {
        PreviewFormat::H265 => "its embedded H.265 (HEIF) preview is not extracted",
        PreviewFormat::JpegXl => "its embedded JPEG XL preview is not extracted",
        PreviewFormat::NotJpeg => "an embedded preview declared a JPEG does not begin with SOI",
        PreviewFormat::Kodak => "an embedded preview of LibRaw's Kodak kinds is not extracted",
        PreviewFormat::DngYcbcr => "its embedded DNG YCbCr preview is not extracted",
        PreviewFormat::X3f => "its embedded Sigma X3F preview is not extracted",
        PreviewFormat::UnreadableBitmap => "an embedded bitmap LibRaw cannot read is not extracted",
        _ => "an embedded preview of unknown format is not extracted",
    }
}

/// The file cannot be read now: gone, on an unmounted volume, or refused.
fn unavailable(record: &FileRecord, kind: std::io::ErrorKind) -> Error {
    Error::source_unavailable(format!(
        "{} is not available: {kind}",
        record.path.display()
    ))
}

/// What a RAW read's failure means for a task: a stopped read is the task's cancellation, a read
/// that failed is the file unavailable, and anything else keeps the kind the RAW crate's error
/// has: a file LibRaw cannot open, or a corrupt image, is `invalid-input`, and one past a limit
/// `resource-limit`.
fn raw_failure(record: &FileRecord, error: RawError, control: &JobControl) -> Error {
    match error {
        RawError::Cancelled => control.cancelled_error(),
        RawError::Io { .. } => {
            Error::source_unavailable(format!("{} cannot be read: {error}", record.path.display()))
        }
        error => raw_error(error),
    }
}

/// Fill `buf` from `offset` of `file`.
fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        match RandomAccess::read_at(file, offset + filled as u64, &mut buf[filled..])? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            read => filled += read,
        }
    }
    Ok(())
}

/// An image decoded as it is stored, before it is fitted and turned upright.
struct Pixels {
    width: u32,
    height: u32,
    rgba: Arc<Vec<u8>>,
    icc: Option<Vec<u8>>,
}

/// `frame` fitted within `side` on its long edge, never enlarged: the proxy's fit rule, so the
/// scale chosen to cover it and the downscale after agree.
fn fitted(frame: (u32, u32), side: u32) -> (u32, u32) {
    ProxyPlan::fit(frame, frame, bounds(side)).map_or(frame, |plan| (plan.width, plan.height))
}

fn bounds(side: u32) -> ProxyBounds {
    ProxyBounds {
        width: side,
        height: side,
    }
}

/// `bytes` up to and including their last EOI marker: what a strict decoder accepts of a camera
/// JPEG with bytes after its image. Bytes with no EOI are left to the decoder to refuse.
pub(crate) fn through_last_eoi(bytes: &[u8]) -> &[u8] {
    match bytes.windows(2).rposition(|pair| pair == [0xff, 0xd9]) {
        Some(at) => &bytes[..at + 2],
        None => bytes,
    }
}

/// Decode a JPEG at the DCT scale that covers its frame fitted within `side`, in strips, checking
/// the task's cancellation between them. Within the JPEG original's limits, checked at the frame's
/// full size before anything scales with it.
fn decode_jpeg(bytes: &[u8], side: u32, control: &JobControl) -> Result<Pixels, Error> {
    let bytes = through_last_eoi(bytes);
    let mut decoder = Decoder::new(bytes, JPEG_LIMITS)?;
    let frame = (decoder.width(), decoder.height());
    decoder.set_scale(Scale::covering(frame, fitted(frame, side)))?;
    let (width, height) = (decoder.width(), decoder.height());
    let stride = width as usize * 4;
    // The one frame a decode allocates, at the scale that covers the tier rather than the frame's
    // size; the downscale reads it once and it is dropped with this task's step.
    let mut rgba = vec![0; Raster::expected_len(width, height)?];
    for strip in rgba.chunks_mut(STRIP_ROWS * stride) {
        control.checkpoint()?;
        decoder.read_rows(strip)?;
    }
    let icc = decoder.icc_profile().map(<[u8]>::to_vec);
    decoder.finish()?;
    Ok(Pixels {
        width,
        height,
        rgba: Arc::new(rgba),
        icc,
    })
}

/// An 8-bit RGB bitmap of `width` × `height` as RGBA.
fn rgb_pixels(width: u32, height: u32, rgb: &[u8]) -> Result<Pixels, Error> {
    let len = Raster::expected_len(width, height)?;
    if rgb.len() != len / 4 * 3 {
        return Err(Error::unsupported_input(
            "an embedded bitmap's length is not three bytes a pixel",
        ));
    }
    let mut rgba = Vec::with_capacity(len);
    for pixel in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(Pixels {
        width,
        height,
        rgba: Arc::new(rgba),
        icc: None,
    })
}

/// An extracted RAW image as pixels: a JPEG decoded at the scale that covers `side`, a bitmap as
/// it is.
fn embedded_pixels(image: EmbeddedImage, side: u32, control: &JobControl) -> Result<Pixels, Error> {
    match image {
        EmbeddedImage::Jpeg(bytes) => decode_jpeg(&bytes, side, control),
        EmbeddedImage::Rgb8 {
            width,
            height,
            pixels,
        } => rgb_pixels(width, height, &pixels),
    }
}

/// Fit `pixels` within `side` with the proxy's box downscale, turn them upright by `orientation`
/// and encode them, keeping their ICC profile.
fn finish(
    pixels: Pixels,
    side: u32,
    orientation: u8,
    origin: PreviewOrigin,
    control: &JobControl,
) -> Result<Made, Error> {
    let Pixels {
        width,
        height,
        rgba,
        icc,
    } = pixels;
    let frame = (width, height);
    let stored = match ProxyPlan::fit(frame, frame, bounds(side)) {
        None => SourceImage {
            width,
            height,
            rgba,
            fingerprint: String::new(),
            orientation: 1,
            capture: Arc::default(),
        },
        Some(plan) => {
            let source = PreviewSource::Jpeg(SourceImage {
                width,
                height,
                rgba,
                fingerprint: String::new(),
                orientation: 1,
                capture: Arc::default(),
            });
            match source.proxy_cancellable(plan, control.render_cancel())? {
                PreviewSource::Jpeg(image) => image,
                PreviewSource::Raw { .. } => {
                    return Err(Error::internal("a byte source downscaled to linear planes"));
                }
            }
        }
    };
    control.checkpoint()?;
    let (upright, width, height) = upright(&stored.rgba, stored.width, stored.height, orientation);
    control.checkpoint()?;
    let mut jpeg = Vec::new();
    luxforge_jpeg::encode(
        &mut jpeg,
        width,
        height,
        &upright,
        &Settings {
            quality: QUALITY,
            chroma: CHROMA,
            segments: &[],
            icc: icc.as_deref(),
            pixels_per_inch: None,
        },
        &mut |_| control.checkpoint(),
    )?;
    Ok(Made {
        jpeg,
        width,
        height,
        origin,
    })
}

/// `rgba`, `width` × `height` as stored, turned upright by EXIF `orientation` through the source
/// decode's own mapping: the same pixels when they are upright already, otherwise one new
/// tier-sized frame, at most `LOUPE_MAX_SIDE` on its long edge.
fn upright(rgba: &[u8], width: u32, height: u32, orientation: u8) -> (Cow<'_, [u8]>, u32, u32) {
    if !(2..=8).contains(&orientation) {
        return (Cow::Borrowed(rgba), width, height);
    }
    let (w, h) = (width as usize, height as usize);
    let (upright_width, upright_height) = if orientation >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (ux, uy) = upright_position(orientation, w, h, x, y);
            let from = (y * w + x) * 4;
            let to = (uy * upright_width + ux) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    (Cow::Owned(out), upright_width as u32, upright_height as u32)
}
