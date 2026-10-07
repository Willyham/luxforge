//! The 100% focus check's pixels (`docs/design/catalog.md`, "Browsing at speed", P6): one rectangle
//! of a frame at full resolution, from the camera's embedded preview when it is full size, decoded
//! for that rectangle alone, and otherwise from a neutral Luxforge development of the frame, made
//! on demand, one RAW at a time, off the editor's source cache.
//!
//! These are the domain functions `preview.region` answers from. The preview lane runs them on its
//! region worker (`regions.rs`), never on the catalog owner thread (performance rule 5), writes
//! the answer's JPEG ([`encode_region`]) and labels it with [`RegionImage::origin`]; its
//! extraction workers develop a RAW with no usable preview through [`developed_preview`].
//!
//! # The frame
//!
//! A [`PixelRect`] is in its image's upright full-resolution pixels: for a JPEG original, its
//! frame after its EXIF orientation; for a RAW, the upright size of Luxforge's own development of
//! it, which is what the fallback shows and what a full-size embedded JPEG approximates (the
//! inventory, `docs/research/embedded-previews.md`, found every full-size preview at 0.99 to 1.00
//! of the visible image). A caller that knows the rectangle against another upright frame — the
//! header's dimensions turned upright, or the loupe's preview — names that frame in
//! [`RegionRequest::frame`].
//!
//! Each path cuts from its own source's upright frame: the JPEG original's, the embedded
//! preview's, or the development's. When the request names a frame of another size, the
//! rectangle's centre is mapped proportionally into the source's frame and its size is kept at
//! 1:1, since a 100% region is never resampled; with no frame named, or the same size, the
//! rectangle is taken as it is. The rectangle is then clamped to the source. The answer reports the
//! rectangle it returns in the frame it cut it from ([`RegionImage::rect`] in
//! [`RegionImage::frame`]). A rectangle of no pixels, one starting outside the request's frame, or
//! one past [`MAX_REGION_BYTES`] is refused.
//!
//! # From the embedded preview
//!
//! A JPEG original is its own full-size image: the region is decoded from the file. For a RAW,
//! [`EmbeddedPreviews`] lists the file's embedded images without unpacking it and extracts the
//! largest JPEG. It is full size when both its edges are at least 95% of the visible image LibRaw
//! would develop, long edge against long edge (the inventory's rule, [`is_full_size`]); a listed
//! size that already fails the rule skips the extraction. Its bytes are cut after their last EOI
//! marker, since Canon CR3, Leica and some DJI previews carry bytes after it and `luxforge-jpeg`
//! refuses a file that does not end at EOI. The rectangle is mapped to the stored (unrotated)
//! preview through the inverse of the file's orientation (every preview is stored in the sensor's
//! orientation), [`RegionDecoder`] decodes exactly that stored rectangle — only its rows and
//! columns, with the iMCU margins it needs to be byte-exact — and the pixels are turned upright.
//! Its origin is [`PreviewOrigin::Embedded`]. A preview smaller than the frame, or none at all, is
//! [`EmbeddedRegion::NotFullSize`], not an error: the caller develops.
//!
//! The pixels are the decoded bytes as the file or the camera stored them, as the loupe shows the
//! preview they come from: no ICC profile is applied or checked.
//!
//! # From a development
//!
//! The fallback shows the frame as the editor shows an unedited RAW: its Original's rendering, the
//! RAW development at the camera's as-shot white balance and nothing else. It never uses the
//! editor's one-slot source cache or its source worker: it reads the file through one bounded read
//! of its own handle ([`read_bounded_file`]), decodes and develops it ([`RawPrepared::decode`]) and
//! renders the Original's stack through the one render entry, on the caller's thread.
//!
//! **One RAW development at a time, process-wide.** Every development goes through one slot
//! ([`Developments`]): a caller that finds another development running waits for it, at most
//! [`MAX_DEVELOPMENT_WAITERS`] of them (a caller past that is refused `resource-limit`), each
//! cancellable while it waits. Nothing polls: a waiter wakes when the running development ends,
//! and when [`wake_development_waiters`] is called, which a job's cancel does after setting the
//! job's flag.
//!
//! **One developed frame kept.** The slot keeps the last development, keyed by its file's path and
//! signature, as upright RGBA8 display bytes (within the evaluated-frame limit,
//! `crate::render::limits::MAX_FRAME_BYTES`), never as float planes, so the next region of the same frame —
//! the pointer moving — is a copy of its rows and develops nothing. It is released before another
//! frame is developed, so two never sit side by side, and by [`release_development`]. A frame the
//! slot keeps answers a region before the embedded preview is listed again. The same development
//! makes the grid and loupe tiers of a RAW that carries no usable preview at all
//! ([`developed_preview`]); its origin is [`PreviewOrigin::Developed`].
//!
//! A camera outside Luxforge's RAW catalog is `unsupported-input`; any other failure of the
//! development keeps its own kind. A JPEG original never needs the fallback.
//!
//! # Cancellation and memory
//!
//! Every pass checks the caller's [`Cancel`]: the region decode and the cut every
//! [`REGION_STRIP_ROWS`] rows, the RAW crate's reads, decode and development through its flag, the
//! render per row or chunk, and a waiter while it waits. A cancelled call is `cancelled` and returns
//! no pixels.
//!
//! The embedded path holds the JPEG original's bytes (within `MAX_JPEG_BYTES`) or the extracted
//! preview (within [`REGION_PREVIEW_BYTES`], LibRaw's buffer and the copy at once while it is
//! extracted), the region itself (within [`MAX_REGION_BYTES`]), one strip of [`REGION_STRIP_ROWS`]
//! stored rows of the region when it is turned, the decoder's one window row, and libjpeg's own
//! buffers, which follow the preview's frame. The development holds, one after another, the
//! file's bytes (within `luxforge_raw::MAX_SOURCE_BYTES`) and the mosaic, the mosaic and the float
//! planes, then the planes and the rendered RGBA8 frame; only the frame is kept.

use crate::{
    Cancel, Error, LayerId, LinearSettings, ModuleRegistry, ProxyBounds, ProxyPlan, RawPayload,
    Recipe, RenderContext, RenderOptions, RenderSource, SnapshotId, SourceImage, SourceTag,
    catalog_types::{FileSignature, LOUPE_MAX_SIDE, PixelRect, PreviewOrigin},
    export::metadata::jpeg_orientation,
    read_bounded_file,
    source::{JPEG_LIMITS, RawPrepared, raw_error, upright_position},
};
use luxforge_jpeg::{Region, RegionDecoder, Settings};
use luxforge_raw::{EmbeddedImage, EmbeddedPreviews, RawError};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
};

/// The most one region's RGBA8 pixels take: 32 MiB, about 2896 px square, the viewport's own
/// region frame. An inset shows far less.
pub(crate) const MAX_REGION_BYTES: u64 = 32 << 20;

/// What one region read may fetch from a RAW file: identify's reads and the one full-size JPEG.
/// The inventory's files read at most 205 KB to list their previews and 12.4 MB to list and
/// extract the largest (Sony A1).
pub(crate) const REGION_READ_BUDGET: u64 = 64 << 20;

/// The largest embedded JPEG a region extracts, LibRaw's buffer and the copy each. The inventory's
/// largest full-size JPEG is 12.3 MB (Sony A1).
pub(crate) const REGION_PREVIEW_BYTES: usize = 48 << 20;

/// A preview is full size when each of its edges is at least this share, in percent, of the
/// visible image's, long against long. Every full-size preview of the inventory is 99 to 100% of
/// the long edge and every other one at most 71%.
const FULL_SIZE_PERCENT: u64 = 95;

/// Rows decoded per region-decode call, copied per cut step, and held in the strip a turned
/// region is decoded through: libjpeg's largest MCU height. Cancellation is checked per strip.
pub(crate) const REGION_STRIP_ROWS: usize = 16;

/// Callers that may wait for the one running RAW development; one more is refused
/// `resource-limit`. Each is a lane worker holding a region or preview job.
pub(crate) const MAX_DEVELOPMENT_WAITERS: usize = 4;

/// The quality a region's answer JPEG is written at, with 4:4:4 chroma so fine detail — what a
/// focus check looks for — is not averaged away.
pub(crate) const REGION_JPEG_QUALITY: u8 = 95;

/// The longest edge [`developed_preview`] makes: the loupe tier's.
pub(crate) const MAX_DEVELOPED_PREVIEW_SIDE: u32 = LOUPE_MAX_SIDE;

/// An upright frame's size in pixels: the frame a rectangle is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FrameSize {
    pub width: u32,
    pub height: u32,
}

impl FrameSize {
    /// The upright size of a `width` × `height` image stored under EXIF `orientation`: 5 to 8
    /// swap its sides.
    pub(crate) fn upright(width: u32, height: u32, orientation: u8) -> Self {
        if orientation >= 5 {
            Self {
                width: height,
                height: width,
            }
        } else {
            Self { width, height }
        }
    }
}

/// One 100% region wanted of a file: which file, as the index knows it, and which rectangle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegionRequest {
    pub path: PathBuf,
    pub kind: SourceTag,
    /// The signature the caller knows the file by. A file whose signature has changed since is
    /// `source-unavailable`: the index must read it again.
    pub signature: FileSignature,
    /// In the image's upright full-resolution pixels, or in [`Self::frame`] when it is named.
    pub rect: PixelRect,
    /// The upright frame `rect` is in, when the caller knows the rectangle against a frame other
    /// than the source's own (see the module's "The frame").
    pub frame: Option<FrameSize>,
}

/// A region's pixels and what they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegionImage {
    /// The rectangle returned, in [`Self::frame`]: the request's, mapped and clamped.
    pub rect: PixelRect,
    /// The upright frame the rectangle was cut from: the JPEG original's, the embedded preview's
    /// or the development's.
    pub frame: FrameSize,
    /// `rect.width` × `rect.height` RGBA8 pixels, rows top to bottom, alpha 255.
    pub rgba: Vec<u8>,
    /// [`PreviewOrigin::Embedded`] for a JPEG original and a RAW's embedded preview,
    /// [`PreviewOrigin::Developed`] for a development.
    pub origin: PreviewOrigin,
}

/// What the embedded path found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EmbeddedRegion {
    Region(RegionImage),
    /// The RAW's largest embedded JPEG is smaller than its visible image, or it carries none (an
    /// H.265-only Canon): the region needs a development. `preview` is the JPEG's size (its frame
    /// header's, or its listed size when that already failed the rule); `visible` is the image
    /// LibRaw would develop, in the sensor's orientation.
    NotFullSize {
        preview: Option<FrameSize>,
        visible: FrameSize,
    },
}

/// A RAW's development downscaled for a grid or loupe tier, upright. Its origin is
/// [`PreviewOrigin::Developed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevelopedPreview {
    pub width: u32,
    pub height: u32,
    /// `width` × `height` RGBA8 pixels; the kept development's own frame, shared, when it already
    /// fits the requested edge.
    pub rgba: Arc<Vec<u8>>,
    /// The development's upright size, the frame a region of it is cut from.
    pub frame: FrameSize,
}

/// The 100% region `request` asks for, from the file's embedded full-size preview or, where it has
/// none, from a development: a frame the development slot already keeps, then the embedded
/// preview, then a new development. See the module documentation.
pub(crate) fn region(request: &RegionRequest, cancel: &Cancel) -> Result<RegionImage, Error> {
    region_in(&DEVELOPMENTS, request, cancel, develop)
}

/// The region from the embedded full-size preview alone: a JPEG original's own pixels, or a RAW's
/// largest embedded JPEG when it is full size, and otherwise [`EmbeddedRegion::NotFullSize`]. The
/// lane answers through [`region`], which takes this path first; the tests take it alone.
#[cfg(test)]
pub(crate) fn embedded_region(
    request: &RegionRequest,
    cancel: &Cancel,
) -> Result<EmbeddedRegion, Error> {
    check_rect(request.rect)?;
    let opened = open(&request.path, &request.signature)?;
    match request.kind {
        SourceTag::Jpeg => jpeg_region(opened, request, cancel).map(EmbeddedRegion::Region),
        SourceTag::Raw => embedded_raw(&opened.file, request, cancel),
    }
}

/// The RAW at `path` developed neutrally and downscaled to `max_side` (1 to
/// [`MAX_DEVELOPED_PREVIEW_SIDE`]) on its long edge, upright, for the grid and loupe tiers of a
/// RAW with no usable embedded preview. The same development answers the frame's 100% regions, and
/// a frame already no larger than `max_side` is the development itself.
pub(crate) fn developed_preview(
    path: &Path,
    signature: &FileSignature,
    max_side: u32,
    cancel: &Cancel,
) -> Result<DevelopedPreview, Error> {
    developed_preview_in(&DEVELOPMENTS, path, signature, max_side, cancel, develop)
}

/// `image` as the baseline JPEG a region's answer names: [`REGION_JPEG_QUALITY`], 4:4:4.
pub(crate) fn encode_region(image: &RegionImage) -> Result<Vec<u8>, Error> {
    encode_rgba(image.rect.width, image.rect.height, &image.rgba)
}

/// `width` × `height` RGBA8 pixels as the baseline JPEG a region or a developed preview is
/// written as: [`REGION_JPEG_QUALITY`], 4:4:4, no metadata.
pub(crate) fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let settings = Settings {
        quality: REGION_JPEG_QUALITY,
        chroma: (1, 1),
        segments: &[],
        icc: None,
        pixels_per_inch: None,
    };
    luxforge_jpeg::encode(&mut out, width, height, rgba, &settings, &mut |_| {
        Ok::<(), Error>(())
    })?;
    Ok(out)
}

/// Wake every caller waiting for the running development, so one whose job was cancelled returns
/// `cancelled` without waiting for it to end. A job's cancel calls this after setting its flag.
pub(crate) fn wake_development_waiters() {
    DEVELOPMENTS.wake();
}

/// Drop the kept development, for when nothing will ask for its regions again (the Select
/// workspace closed). A caller cutting a region from it keeps it until it is done.
pub(crate) fn release_development() {
    DEVELOPMENTS.release();
}

/// Develop the file at `path` through the process's one development slot with `develop`, as a
/// test holds the slot while the lane's workers wait on it.
#[cfg(test)]
pub(crate) fn develop_in_slot(
    path: &Path,
    signature: &FileSignature,
    develop: impl FnOnce(File, &Cancel) -> Result<Developed, Error>,
) -> Result<(), Error> {
    let opened = open(path, signature)?;
    DEVELOPMENTS
        .frame(path, opened, &Cancel::new(), develop)
        .map(drop)
}

/// How many callers wait for the process's development now.
#[cfg(test)]
pub(crate) fn development_waiters() -> usize {
    DEVELOPMENTS.waiting()
}

/// Whether an embedded preview of `preview`'s size is full size for a `visible` image: each of its
/// edges at least [`FULL_SIZE_PERCENT`] of the visible image's, long edge against long edge, so a
/// preview stored in either orientation compares the same way. A visible image of no pixels has
/// no full-size preview.
pub(crate) fn is_full_size(preview: FrameSize, visible: FrameSize) -> bool {
    let edges = |size: FrameSize| {
        (
            u64::from(size.width.max(size.height)),
            u64::from(size.width.min(size.height)),
        )
    };
    let ((long, short), (visible_long, visible_short)) = (edges(preview), edges(visible));
    visible_short > 0
        && long * 100 >= visible_long * FULL_SIZE_PERCENT
        && short * 100 >= visible_short * FULL_SIZE_PERCENT
}

/// [`region`] through `developments`, developing with `develop`.
fn region_in(
    developments: &Developments,
    request: &RegionRequest,
    cancel: &Cancel,
    develop: impl FnOnce(File, &Cancel) -> Result<Developed, Error>,
) -> Result<RegionImage, Error> {
    check_rect(request.rect)?;
    let opened = open(&request.path, &request.signature)?;
    match request.kind {
        SourceTag::Jpeg => jpeg_region(opened, request, cancel),
        SourceTag::Raw => {
            if let Some(frame) = developments.kept(&request.path, &opened.signature) {
                return developed_cut(&frame, request, cancel);
            }
            match embedded_raw(&opened.file, request, cancel)? {
                EmbeddedRegion::Region(image) => Ok(image),
                EmbeddedRegion::NotFullSize { .. } => {
                    let frame = developments.frame(&request.path, opened, cancel, develop)?;
                    developed_cut(&frame, request, cancel)
                }
            }
        }
    }
}

/// The region from a neutral development of the RAW through `developments`, developing with
/// `develop`: one development at a time, kept for the next region of the same frame. A JPEG
/// original is refused: it is its own full-size image. The lane answers through [`region`], which
/// develops when the embedded preview will not do; the tests take this path alone.
#[cfg(test)]
fn developed_region_in(
    developments: &Developments,
    request: &RegionRequest,
    cancel: &Cancel,
    develop: impl FnOnce(File, &Cancel) -> Result<Developed, Error>,
) -> Result<RegionImage, Error> {
    check_rect(request.rect)?;
    if request.kind != SourceTag::Raw {
        return Err(Error::validation(
            "a JPEG original is its own full-size image and is never developed",
        ));
    }
    let opened = open(&request.path, &request.signature)?;
    let frame = developments.frame(&request.path, opened, cancel, develop)?;
    developed_cut(&frame, request, cancel)
}

/// [`developed_preview`] through `developments`, developing with `develop`.
fn developed_preview_in(
    developments: &Developments,
    path: &Path,
    signature: &FileSignature,
    max_side: u32,
    cancel: &Cancel,
    develop: impl FnOnce(File, &Cancel) -> Result<Developed, Error>,
) -> Result<DevelopedPreview, Error> {
    if !(1..=MAX_DEVELOPED_PREVIEW_SIDE).contains(&max_side) {
        return Err(Error::validation(format!(
            "a developed preview's long edge is 1 to {MAX_DEVELOPED_PREVIEW_SIDE} px"
        )));
    }
    let opened = open(path, signature)?;
    let frame = developments.frame(path, opened, cancel, develop)?;
    downscaled(&frame, max_side, cancel)
}

/// A file opened for one request, and the signature its handle has now.
struct Opened {
    file: File,
    signature: FileSignature,
}

/// Open `path` and check it is still the file `expected` names: a regular file whose signature is
/// unchanged ([`FileSignature::unchanged`]). A file that is gone is `source-unavailable`, as is
/// one that changed; one that cannot be read is `read-error`.
fn open(path: &Path, expected: &FileSignature) -> Result<Opened, Error> {
    let failed = |error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => Error::source_unavailable("the file is gone"),
        kind => Error::file_access(kind.to_string()),
    };
    let file = File::open(path).map_err(failed)?;
    let metadata = file.metadata().map_err(failed)?;
    if !metadata.is_file() {
        return Err(Error::unsupported_input("expected a regular file"));
    }
    let signature = FileSignature::of(&metadata);
    if !expected.unchanged(&signature) {
        return Err(Error::source_unavailable(
            "the file changed since it was indexed",
        ));
    }
    Ok(Opened { file, signature })
}

/// A rectangle of at least one pixel whose RGBA8 pixels fit [`MAX_REGION_BYTES`]: what the owner
/// checks before it queues a region, and every entry here again.
pub(crate) fn check_rect(rect: PixelRect) -> Result<(), Error> {
    if rect.width == 0 || rect.height == 0 {
        return Err(Error::validation("a region needs at least one pixel"));
    }
    if u64::from(rect.width) * u64::from(rect.height) * 4 > MAX_REGION_BYTES {
        return Err(Error::resource_limit(format!(
            "a region's pixels exceed {} MiB",
            MAX_REGION_BYTES >> 20
        )));
    }
    Ok(())
}

/// The rectangle of a `source` frame that `rect` of `frame` names (the source's own frame when
/// none is named): its centre mapped proportionally, its size kept, then clamped to the source.
/// A rectangle starting outside `frame` is refused.
fn source_rect(
    rect: PixelRect,
    frame: Option<FrameSize>,
    source: FrameSize,
) -> Result<PixelRect, Error> {
    let frame = frame.unwrap_or(source);
    if source.width == 0 || source.height == 0 || frame.width == 0 || frame.height == 0 {
        return Err(Error::validation("a region's frame has no pixels"));
    }
    if rect.x >= frame.width || rect.y >= frame.height {
        return Err(Error::validation("a region starts outside its frame"));
    }
    let x = mapped_start(rect.x, rect.width, frame.width, source.width);
    let y = mapped_start(rect.y, rect.height, frame.height, source.height);
    Ok(PixelRect {
        x,
        y,
        width: rect.width.min(source.width - x),
        height: rect.height.min(source.height - y),
    })
}

/// Where a span of `size` starts in a side of `to` pixels when its centre sits at the fraction of
/// that side that the span starting at `start` has of a side of `from` pixels: `start` itself when
/// the sides agree, and always inside the side (`start < from` gives a result below `to`).
fn mapped_start(start: u32, size: u32, from: u32, to: u32) -> u32 {
    if from == to {
        return start;
    }
    // Twice the centre, so the arithmetic stays in integers.
    let centre = (2 * u64::from(start) + u64::from(size)) * u64::from(to) / u64::from(from);
    (centre.saturating_sub(u64::from(size)) / 2).min(u64::from(to) - 1) as u32
}

/// The region of a JPEG original: the file read once through its bounded read, its EXIF
/// orientation read from the same bytes.
fn jpeg_region(
    mut opened: Opened,
    request: &RegionRequest,
    cancel: &Cancel,
) -> Result<RegionImage, Error> {
    cancel.check()?;
    let bytes = read_bounded_file(&mut opened.file)?;
    drop(opened);
    let header = luxforge_jpeg::header(&bytes)?;
    let stored = FrameSize {
        width: header.width,
        height: header.height,
    };
    upright_region(&bytes, stored, jpeg_orientation(&bytes), request, cancel)
}

/// The embedded path for the RAW read through `file`: its largest embedded JPEG's region when it
/// is full size, else [`EmbeddedRegion::NotFullSize`].
fn embedded_raw(
    file: &File,
    request: &RegionRequest,
    cancel: &Cancel,
) -> Result<EmbeddedRegion, Error> {
    let failed = |error: RawError| cancelled_or(cancel, raw_error(error));
    let mut previews =
        EmbeddedPreviews::open(file, REGION_READ_BUDGET, cancel.flag()).map_err(failed)?;
    let listing = previews.listing();
    let visible = FrameSize {
        width: listing.width,
        height: listing.height,
    };
    let orientation = listing.orientation;
    let Some(largest) = listing.largest_jpeg().copied() else {
        return Ok(EmbeddedRegion::NotFullSize {
            preview: None,
            visible,
        });
    };
    let listed = FrameSize {
        width: largest.width,
        height: largest.height,
    };
    // A listed size that already fails the rule saves reading the preview; a listing of 0 × 0 (a
    // Canon CR3's full-size JPEG) says nothing, so the frame header decides.
    if listed.width > 0 && listed.height > 0 && !is_full_size(listed, visible) {
        return Ok(EmbeddedRegion::NotFullSize {
            preview: Some(listed),
            visible,
        });
    }
    let EmbeddedImage::Jpeg(mut jpeg) = previews
        .extract(largest.index, REGION_PREVIEW_BYTES, cancel.flag())
        .map_err(failed)?
    else {
        return Err(Error::internal(
            "an embedded JPEG was extracted as a bitmap",
        ));
    };
    // LibRaw's handle and its native memory go before the decode.
    drop(previews);
    cut_after_last_eoi(&mut jpeg);
    let header = luxforge_jpeg::header(&jpeg)?;
    let stored = FrameSize {
        width: header.width,
        height: header.height,
    };
    if !is_full_size(stored, visible) {
        return Ok(EmbeddedRegion::NotFullSize {
            preview: Some(stored),
            visible,
        });
    }
    upright_region(&jpeg, stored, orientation, request, cancel).map(EmbeddedRegion::Region)
}

/// Drop whatever follows the last EOI marker: `luxforge-jpeg` reads a file that ends at EOI, and
/// libjpeg never reads past the first one, so bytes between the two are never seen.
fn cut_after_last_eoi(jpeg: &mut Vec<u8>) {
    if let Some(at) = jpeg.windows(2).rposition(|pair| pair == [0xff, 0xd9]) {
        jpeg.truncate(at + 2);
    }
}

/// `request`'s rectangle of the JPEG `jpeg`, stored `stored` in size under EXIF `orientation`,
/// decoded for that rectangle alone and turned upright.
fn upright_region(
    jpeg: &[u8],
    stored: FrameSize,
    orientation: u8,
    request: &RegionRequest,
    cancel: &Cancel,
) -> Result<RegionImage, Error> {
    let frame = FrameSize::upright(stored.width, stored.height, orientation);
    let rect = source_rect(request.rect, request.frame, frame)?;
    let rgba = decode_region(jpeg, stored, orientation, rect, cancel)?;
    Ok(RegionImage {
        rect,
        frame,
        rgba,
        origin: PreviewOrigin::Embedded,
    })
}

/// The stored rectangle whose pixels `orientation` turns into the upright `rect`: the stored
/// positions of the rectangle's first and last pixels, through the orientation's inverse.
fn stored_region(stored: FrameSize, orientation: u8, rect: PixelRect) -> Region {
    let upright = FrameSize::upright(stored.width, stored.height, orientation);
    // Every orientation is its own inverse but the two quarter turns, which undo each other.
    let inverse = match orientation {
        6 => 8,
        8 => 6,
        other => other,
    };
    let (width, height) = (upright.width as usize, upright.height as usize);
    let (x0, y0) = upright_position(inverse, width, height, rect.x as usize, rect.y as usize);
    let (x1, y1) = upright_position(
        inverse,
        width,
        height,
        (rect.x + rect.width - 1) as usize,
        (rect.y + rect.height - 1) as usize,
    );
    Region {
        x: x0.min(x1) as u32,
        y: y0.min(y1) as u32,
        width: (x0.abs_diff(x1) + 1) as u32,
        height: (y0.abs_diff(y1) + 1) as u32,
    }
}

/// Decode the upright `rect` of `jpeg`, stored `stored` in size under EXIF `orientation`: the
/// stored rectangle alone, by [`RegionDecoder`], [`REGION_STRIP_ROWS`] rows at a time, turned
/// upright into the rectangle's own allocation. An upright file decodes straight into it; a turned
/// one through one strip of the stored rectangle's rows.
fn decode_region(
    jpeg: &[u8],
    stored: FrameSize,
    orientation: u8,
    rect: PixelRect,
    cancel: &Cancel,
) -> Result<Vec<u8>, Error> {
    let region = stored_region(stored, orientation, rect);
    let mut decoder = RegionDecoder::new(jpeg, JPEG_LIMITS, region)?;
    let (width, height) = (region.width as usize, region.height as usize);
    let stride = width * 4;
    let mut rgba = vec![0; stride * height];
    allocations::note(rgba.len());
    if orientation == 1 {
        for strip in rgba.chunks_mut(REGION_STRIP_ROWS * stride) {
            cancel.check()?;
            decoder.read_rows(strip)?;
        }
        return Ok(rgba);
    }
    let upright_width = rect.width as usize;
    let mut strip = vec![0; REGION_STRIP_ROWS.min(height) * stride];
    allocations::note(strip.len());
    for first in (0..height).step_by(REGION_STRIP_ROWS) {
        cancel.check()?;
        let rows = REGION_STRIP_ROWS.min(height - first);
        let strip = &mut strip[..rows * stride];
        decoder.read_rows(strip)?;
        // Column by column, so a quarter turn writes each decoded column's rows as one run of an
        // upright row.
        for x in 0..width {
            for row in 0..rows {
                let (ux, uy) = upright_position(orientation, width, height, x, first + row);
                let from = row * stride + x * 4;
                let to = (uy * upright_width + ux) * 4;
                rgba[to..to + 4].copy_from_slice(&strip[from..from + 4]);
            }
        }
    }
    Ok(rgba)
}

/// `request`'s region of a kept development.
fn developed_cut(
    frame: &DevelopedFrame,
    request: &RegionRequest,
    cancel: &Cancel,
) -> Result<RegionImage, Error> {
    let rect = source_rect(request.rect, request.frame, frame.size)?;
    let row = rect.width as usize * 4;
    let stride = frame.size.width as usize * 4;
    let mut rgba = Vec::with_capacity(row * rect.height as usize);
    for (index, y) in (rect.y..rect.y + rect.height).enumerate() {
        if index % REGION_STRIP_ROWS == 0 {
            cancel.check()?;
        }
        let start = y as usize * stride + rect.x as usize * 4;
        rgba.extend_from_slice(&frame.rgba[start..start + row]);
    }
    Ok(RegionImage {
        rect,
        frame: frame.size,
        rgba,
        origin: PreviewOrigin::Developed,
    })
}

/// A kept development downscaled to `max_side` on its long edge through the view's area average
/// of display bytes, or the development itself when it already fits.
fn downscaled(
    frame: &DevelopedFrame,
    max_side: u32,
    cancel: &Cancel,
) -> Result<DevelopedPreview, Error> {
    let size = (frame.size.width, frame.size.height);
    let bounds = ProxyBounds {
        width: max_side,
        height: max_side,
    };
    let Some(plan) = ProxyPlan::fit(size, size, bounds) else {
        return Ok(DevelopedPreview {
            width: frame.size.width,
            height: frame.size.height,
            rgba: Arc::clone(&frame.rgba),
            frame: frame.size,
        });
    };
    let source = SourceImage {
        width: frame.size.width,
        height: frame.size.height,
        rgba: Arc::clone(&frame.rgba),
        fingerprint: String::new(),
        orientation: 1,
        capture: Arc::default(),
    };
    let scaled = crate::proxy::downscale_bytes(&source, plan, cancel)?;
    Ok(DevelopedPreview {
        width: scaled.width,
        height: scaled.height,
        rgba: scaled.rgba,
        frame: frame.size,
    })
}

/// `error`, or `cancelled` when `cancel` is set: the RAW crate and the camera conversion report a
/// cancellation of their own kind, which a caller that cancelled reads as `cancelled`.
fn cancelled_or(cancel: &Cancel, error: Error) -> Error {
    match cancel.check() {
        Err(cancelled) => cancelled,
        Ok(()) => error,
    }
}

/// What one development makes: its upright frame as RGBA8 display bytes.
pub(crate) struct Developed {
    pub size: FrameSize,
    pub rgba: Arc<Vec<u8>>,
}

/// The production development: `file` read through one bounded read, decoded and developed at the
/// camera's as-shot white balance, and its Original's stack — the RAW development alone —
/// rendered to display bytes through the one render entry, exactly as the editor renders an
/// unedited RAW. Nothing here names the editor's source cache or its worker.
fn develop(mut file: File, cancel: &Cancel) -> Result<Developed, Error> {
    let bytes = read_bounded_file(&mut file)?;
    drop(file);
    cancel.check()?;
    // No fingerprint: nothing here is cached by content, and the kept frame is keyed by the
    // file's path and signature, so the bytes are never hashed.
    let prepared = RawPrepared::decode(bytes, String::new(), None, cancel.flag())?;
    let metadata = prepared.sensor.metadata();
    let original = Recipe::default().with_layer_inserted(
        0,
        RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz)?.layer(LayerId::new()),
    )?;
    // The mosaic and the capture metadata go before the frame is allocated.
    let RawPrepared {
        sensor,
        linear,
        capture,
        ..
    } = prepared;
    drop((sensor, capture));
    let linear = linear.ok_or_else(|| Error::internal("a RAW development without planes"))?;
    let registry = ModuleRegistry::builtin();
    let context = RenderContext::new();
    let raster = crate::render::render(
        &registry,
        RenderSource::Linear {
            image: &linear,
            settings: LinearSettings::default(),
        },
        &original,
        RenderOptions::exact(cancel),
        &context,
    )?
    .frame(SnapshotId::new())?;
    drop(linear);
    Ok(Developed {
        size: FrameSize {
            width: raster.width,
            height: raster.height,
        },
        rgba: raster.rgba,
    })
}

/// A development kept by the slot: the file it is of, as its path and signature, and its upright
/// frame as display bytes.
struct DevelopedFrame {
    path: PathBuf,
    signature: FileSignature,
    size: FrameSize,
    rgba: Arc<Vec<u8>>,
}

impl DevelopedFrame {
    fn is_of(&self, path: &Path, signature: &FileSignature) -> bool {
        self.path == path && self.signature == *signature
    }
}

/// The process's one RAW development slot. Every development of the 100% region and of the
/// developed tiers runs through [`DEVELOPMENTS`].
static DEVELOPMENTS: Developments = Developments::new();

/// One development at a time, and the one frame kept: see the module's "From a development".
struct Developments {
    slot: Mutex<Slot>,
    /// Signalled when a development ends and by [`Self::wake`].
    changed: Condvar,
}

struct Slot {
    /// Whether a development runs now.
    running: bool,
    /// Callers waiting for it, at most [`MAX_DEVELOPMENT_WAITERS`].
    waiting: usize,
    /// The last development, until another is started or it is released.
    kept: Option<Arc<DevelopedFrame>>,
}

impl Developments {
    const fn new() -> Self {
        Self {
            slot: Mutex::new(Slot {
                running: false,
                waiting: 0,
                kept: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The kept development of the file at `path` with `signature`, if it is the one kept.
    fn kept(&self, path: &Path, signature: &FileSignature) -> Option<Arc<DevelopedFrame>> {
        self.lock()
            .kept
            .as_ref()
            .filter(|frame| frame.is_of(path, signature))
            .cloned()
    }

    /// The development of `opened`, the file at `path`: the kept one when it is of this file, else
    /// a new one by `develop`, once no other development runs. A caller waits while another runs,
    /// and takes that development if it turns out to be of this file; it is refused past
    /// [`MAX_DEVELOPMENT_WAITERS`], and returns `cancelled` once `cancel` is set and it is woken.
    fn frame(
        &self,
        path: &Path,
        opened: Opened,
        cancel: &Cancel,
        develop: impl FnOnce(File, &Cancel) -> Result<Developed, Error>,
    ) -> Result<Arc<DevelopedFrame>, Error> {
        let Opened { file, signature } = opened;
        let mut slot = self.lock();
        loop {
            if let Some(kept) = slot
                .kept
                .as_ref()
                .filter(|kept| kept.is_of(path, &signature))
            {
                return Ok(Arc::clone(kept));
            }
            cancel.check()?;
            if !slot.running {
                break;
            }
            if slot.waiting >= MAX_DEVELOPMENT_WAITERS {
                return Err(Error::resource_limit(format!(
                    "at most {MAX_DEVELOPMENT_WAITERS} callers wait for the one RAW development"
                )));
            }
            slot.waiting += 1;
            slot = self
                .changed
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
            slot.waiting -= 1;
        }
        slot.running = true;
        // The kept frame goes before another is developed, so the two never sit side by side at
        // the development's peak; a caller still cutting a region from it holds it until done.
        slot.kept = None;
        drop(slot);
        // Ends the running development on every way out, a panic included.
        let running = Running(self);
        let developed = develop(file, cancel).map_err(|error| cancelled_or(cancel, error))?;
        let expected = u64::from(developed.size.width) * u64::from(developed.size.height) * 4;
        if developed.rgba.len() as u64 != expected {
            return Err(Error::internal(
                "a development's pixels do not match its size",
            ));
        }
        let frame = Arc::new(DevelopedFrame {
            path: path.to_path_buf(),
            signature,
            size: developed.size,
            rgba: developed.rgba,
        });
        // Kept before the waiters wake, so one waiting for this file takes it.
        self.lock().kept = Some(Arc::clone(&frame));
        drop(running);
        Ok(frame)
    }

    /// Wake every waiter to look at its cancel token again.
    fn wake(&self) {
        let _slot = self.lock();
        self.changed.notify_all();
    }

    /// Drop the kept development.
    fn release(&self) {
        self.lock().kept = None;
    }

    /// How many callers wait now.
    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.lock().waiting
    }
}

/// The running development's hold on the slot: dropping it ends the development and wakes every
/// waiter.
struct Running<'a>(&'a Developments);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.lock().running = false;
        self.0.changed.notify_all();
    }
}

/// The buffers a region decode allocates on this thread, recorded for the test that proves it
/// allocates the rectangle and not the frame. Nothing is recorded outside the tests.
mod allocations {
    #[cfg(test)]
    thread_local! {
        static RECORDED: std::cell::RefCell<Option<Vec<usize>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Note one buffer of `bytes`.
    #[cfg_attr(not(test), allow(unused_variables))]
    pub(super) fn note(bytes: usize) {
        #[cfg(test)]
        RECORDED.with_borrow_mut(|recorded| {
            if let Some(recorded) = recorded {
                recorded.push(bytes);
            }
        });
    }

    /// `work`'s result, with the size of every buffer noted on this thread while it ran.
    #[cfg(test)]
    pub(super) fn record<T>(work: impl FnOnce() -> T) -> (T, Vec<usize>) {
        RECORDED.with_borrow_mut(|recorded| *recorded = Some(Vec::new()));
        let result = work();
        let recorded = RECORDED.with_borrow_mut(Option::take).unwrap_or_default();
        (result, recorded)
    }
}

#[cfg(test)]
mod preview_region;
