//! Embedded previews: the images a RAW file carries beside its mosaic, listed and extracted
//! without unpacking it.
//!
//! [`EmbeddedPreviews::open`] runs LibRaw's identify through a native datastream whose every fetch
//! is a positional read of the caller's [`RandomAccess`] source, so a preview costs the reads of
//! the container's headers and of the one image extracted, never the whole file. Any RAW LibRaw
//! identifies is listed, whatever its camera: the camera catalog and its mode checks belong to
//! [`crate::RawSource::decode`] alone. The mosaic is never unpacked, by LibRaw or by RawSpeed.
//!
//! Every byte fetched counts against the handle's read budget, and an extraction against its byte
//! limit, both checked before the work they bound; each fetch checks the caller's cancel token.

use crate::{
    CancelCallback, RawError, c_text, cancelled, exif_orientation,
    limits::{MAX_EMBEDDED_IMAGE_BYTES, MAX_EMBEDDED_READ_BUDGET},
    native_result,
    native_status::NativeStatus,
    token,
};
use serde::Serialize;
use std::{
    cell::{Cell, RefCell},
    ffi::{c_char, c_int, c_void},
    fs::File,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    sync::{Arc, atomic::AtomicBool, atomic::Ordering},
};

/// The native datastream's block: identify's many 1–4-byte reads fetch the 4 KiB-aligned block
/// that holds them, so each costs one positional read per block rather than per call. A read of
/// at least a block goes straight to its destination.
const READ_BLOCK: u32 = 16 * 1024;
/// How many blocks the stream keeps, the least recently read replaced first (128 KiB a handle):
/// identify jumps between a directory and the values it points to elsewhere, and one block
/// fetched both again at every jump. Over the inventory's 120 files
/// (`docs/research/embedded-previews.md`), opening and listing read 120 KB a file on average and
/// 204 KB at most in 6.5 fetches, against 313 and 836 KB in 20 with one block; sixteen blocks
/// read no less, 4 KiB blocks read 29% less in 57% more fetches, and 64 KiB blocks twice as much.
const READ_BLOCKS: u32 = 8;

/// A source read by position, which [`EmbeddedPreviews`] reads through LibRaw's identify and its
/// thumbnail extraction. Implemented for a [`File`] (`read_at` on Unix, `seek_read` on Windows,
/// which also moves the file's cursor), for bytes already in memory (`[u8]`, `Vec<u8>`), and
/// through references, boxes and `Arc`s of either, so one path serves a file and a bounded read
/// already made.
pub trait RandomAccess {
    /// The source's length in bytes, read once when a handle opens.
    fn size(&self) -> io::Result<u64>;
    /// Read up to `buf.len()` bytes at `offset`, returning how many were read; 0 only at the end.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

impl RandomAccess for File {
    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }
    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }
    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }
}

impl RandomAccess for [u8] {
    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_or(self.len(), |start| start.min(self.len()));
        let n = buf.len().min(self.len() - start);
        buf[..n].copy_from_slice(&self[start..start + n]);
        Ok(n)
    }
}

impl RandomAccess for Vec<u8> {
    fn size(&self) -> io::Result<u64> {
        self.as_slice().size()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.as_slice().read_at(offset, buf)
    }
}

macro_rules! forward_random_access {
    ($($wrapper:ty),*) => {$(
        impl<T: RandomAccess + ?Sized> RandomAccess for $wrapper {
            fn size(&self) -> io::Result<u64> {
                (**self).size()
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
                (**self).read_at(offset, buf)
            }
        }
    )*};
}
forward_random_access!(&T, Box<T>, Arc<T>);

/// What an embedded image is, from LibRaw's thumbnail list and, for an item LibRaw declares a
/// JPEG, the first bytes stored at its offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PreviewFormat {
    /// A JPEG: LibRaw declares one and its stored bytes begin with the SOI marker.
    Jpeg,
    /// Uncompressed 8-bit samples, one or three channels (LibRaw's layer and PPM kinds).
    /// Extracted as 8-bit RGB.
    Bitmap { channels: u8 },
    /// Uncompressed 16-bit samples, one or three channels (LibRaw's PPM16 kind, which it reads
    /// only from Imacon files). Extracted as 8-bit RGB: LibRaw keeps each sample's high byte.
    Bitmap16 { channels: u8 },
    /// Rollei's RGB 5-6-5 bitmap. Extracted as 8-bit RGB, which LibRaw converts it to.
    Rollei,
    /// Canon's H.265 (HEIF) preview in a CR3 recorded in HEIF mode: LibRaw declares a JPEG, the
    /// stored bytes are H.265. Not extracted.
    H265,
    /// A JPEG XL preview, which LibRaw lists only when asked to (the adapter never asks). Not
    /// extracted.
    JpegXl,
    /// LibRaw declares a JPEG, but the stored bytes do not begin with SOI: LibRaw lists every
    /// TIFF preview whose compression it does not otherwise name (deflate or LZW among them) as a
    /// JPEG. Not extracted.
    NotJpeg,
    /// One of LibRaw's Kodak kinds: a Kodak thumbnail, or any uncompressed TIFF preview of more
    /// than 8 bits a sample (Canon CR2's 16-bit RGB image among them), which LibRaw decodes
    /// through a small development of its own. Not extracted.
    Kodak,
    /// A DNG YCbCr bitmap, which LibRaw 0.22.2 refuses. Not extracted.
    DngYcbcr,
    /// A Sigma X3F preview; this LibRaw is built without X3F support. Not extracted.
    X3f,
    /// A bitmap LibRaw would not read as its pixels: a channel count other than one or three,
    /// samples of other than 8 bits in a kind it reads as 8-bit, or a declared length other than
    /// its pixels (a bitmap in several strips). Not extracted.
    UnreadableBitmap,
    /// A format LibRaw does not name. Not extracted.
    Unknown,
}

impl PreviewFormat {
    /// Whether [`EmbeddedPreviews::extract`] hands this format over.
    pub fn extractable(self) -> bool {
        matches!(
            self,
            Self::Jpeg | Self::Bitmap { .. } | Self::Bitmap16 { .. } | Self::Rollei
        )
    }

    /// Why an extraction of this format is refused, for [`RawError::UnsupportedCompression`].
    fn refusal(self) -> &'static str {
        match self {
            Self::H265 => "an embedded H.265 (HEIF) preview is not extracted",
            Self::JpegXl => "an embedded JPEG XL preview is not extracted",
            Self::NotJpeg => {
                "an embedded preview LibRaw declares a JPEG does not begin with SOI and is not \
                 extracted"
            }
            Self::Kodak => "an embedded preview of LibRaw's Kodak kinds is not extracted",
            Self::DngYcbcr => "an embedded DNG YCbCr preview is not extracted",
            Self::X3f => "an embedded Sigma X3F preview is not extracted",
            Self::UnreadableBitmap => "an embedded bitmap LibRaw cannot read is not extracted",
            Self::Unknown => "an embedded preview of unknown format is not extracted",
            Self::Jpeg | Self::Bitmap { .. } | Self::Bitmap16 { .. } | Self::Rollei => {
                "embedded preview extracted"
            }
        }
    }

    /// The format of one native list item.
    fn of(item: &NativeItem) -> Self {
        // LibRaw 0.22.2's LibRaw_internal_thumbnail_formats.
        const KODAK_THUMB: u32 = 1;
        const KODAK_RGB: u32 = 3;
        const JPEG: u32 = 4;
        const LAYER: u32 = 5;
        const ROLLEI: u32 = 6;
        const PPM: u32 = 7;
        const PPM16: u32 = 8;
        const X3F: u32 = 9;
        const DNG_YCBCR: u32 = 10;
        const JPEGXL: u32 = 11;
        let channels = ((item.misc >> 5) & 7) as u8;
        let bits = item.misc & 31;
        let one_or_three = channels == 1 || channels == 3;
        match item.format {
            JPEG if item.head[..2] == [0xff, 0xd8] => Self::Jpeg,
            JPEG if item.head[..3] == [0, 0, 0] && &item.head[4..] == b"CISZ" => Self::H265,
            JPEG => Self::NotJpeg,
            LAYER if one_or_three => Self::Bitmap { channels },
            PPM if one_or_three
                && bits == 8
                && (item.length == 0
                    || u64::from(item.length)
                        == u64::from(channels)
                            * u64::from(item.width)
                            * u64::from(item.height)) =>
            {
                Self::Bitmap { channels }
            }
            PPM16 if one_or_three && bits <= 16 => Self::Bitmap16 { channels },
            LAYER | PPM | PPM16 => Self::UnreadableBitmap,
            ROLLEI => Self::Rollei,
            KODAK_THUMB..=KODAK_RGB => Self::Kodak,
            X3F => Self::X3f,
            DNG_YCBCR => Self::DngYcbcr,
            JPEGXL => Self::JpegXl,
            _ => Self::Unknown,
        }
    }
}

/// One image in LibRaw's thumbnail list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EmbeddedPreview {
    /// The item's position in LibRaw's list, which [`EmbeddedPreviews::extract`] takes.
    pub index: usize,
    pub format: PreviewFormat,
    /// The dimensions the container declares, 0 where LibRaw read none. A JPEG's own frame header
    /// may differ; the preview lane decodes it to know.
    pub width: u32,
    pub height: u32,
    /// The stored byte length the container declares; 0 for a bitmap that declares none.
    pub bytes: u64,
    /// The item's own orientation as EXIF 1–8, where LibRaw read the item's IFD: 1 also when that
    /// IFD had no Orientation tag. `None` for an item found outside an IFD (LibRaw's `0xffff`).
    /// Every preview in the inventory is stored in the sensor's orientation, which the listing's
    /// `orientation` turns upright.
    pub orientation: Option<u8>,
    /// Where the stored bytes start: a detail of the container, kept for tests.
    #[serde(skip)]
    offset: u64,
}

/// What identify read of a RAW file and the embedded images it listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewListing {
    /// LibRaw's normalized make and model.
    pub make: String,
    pub model: String,
    /// The visible image LibRaw would develop (`sizes.width` × `sizes.height`), in the sensor's
    /// orientation: what a full-size preview matches.
    pub width: u32,
    pub height: u32,
    /// The whole sensor frame (`sizes.raw_width` × `sizes.raw_height`).
    pub raw_width: u32,
    pub raw_height: u32,
    /// The file's EXIF orientation 1–8, from LibRaw's flip.
    pub orientation: u8,
    /// Every image LibRaw listed, at most eight, in its list order. LibRaw's placeholder for a
    /// file with no thumbnail (no offset and no length) is left out.
    pub previews: Vec<EmbeddedPreview>,
}

impl PreviewListing {
    /// The largest JPEG, by stored length. Listed dimensions cannot rank them: LibRaw lists a
    /// Canon CR3's full-size JPEG as 0 × 0. Over the inventory's 117 files with a JPEG
    /// (`docs/research/embedded-previews.md`), the longest was the largest in pixels every time;
    /// the largest by listed dimensions was not in 17.
    pub fn largest_jpeg(&self) -> Option<&EmbeddedPreview> {
        self.previews
            .iter()
            .filter(|preview| preview.format == PreviewFormat::Jpeg)
            .max_by_key(|preview| preview.bytes)
    }
}

/// An extracted embedded image, owned by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedImage {
    /// A JPEG's bytes exactly as stored in the file, beginning with SOI. Its validity is the
    /// decoder's to judge.
    Jpeg(Vec<u8>),
    /// An 8-bit RGB bitmap, rows top to bottom as stored: a one-channel bitmap has its value
    /// repeated in each channel.
    Rgb8 {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
}

/// One entry of the native thumbnail list; `LfPreviewItem` in `native/embedded.cpp`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeItem {
    offset: u64,
    length: u32,
    format: u32,
    width: u32,
    height: u32,
    flip: u32,
    misc: u32,
    head: [u8; 8],
}
const _: () = assert!(std::mem::size_of::<NativeItem>() == 40);

/// `LfPreviewList` in `native/embedded.cpp`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeList {
    make: [c_char; 64],
    model: [c_char; 64],
    width: u32,
    height: u32,
    raw_width: u32,
    raw_height: u32,
    flip: u32,
    count: u32,
    items: [NativeItem; 8],
}
const _: () = assert!(std::mem::size_of::<NativeList>() == 472);

impl NativeList {
    fn blank() -> Self {
        // SAFETY: an all-zero NativeList is valid: integers, byte arrays and C chars.
        unsafe { std::mem::zeroed() }
    }
}

/// `LfPreviewImage` in `native/embedded.cpp`: an extracted image borrowed from LibRaw.
#[repr(C)]
struct NativeImage {
    data: *const u8,
    length: u64,
    kind: u32,
    width: u32,
    height: u32,
    colors: u32,
}
const _: () = assert!(std::mem::size_of::<NativeImage>() == 32);
const NATIVE_JPEG: u32 = 1;
const NATIVE_BITMAP: u32 = 2;

/// `LfReadAt` in `native/embedded.cpp`.
type ReadAt = extern "C" fn(*mut c_void, u64, *mut u8, usize) -> c_int;

unsafe extern "C" {
    fn lf_preview_open(
        read: ReadAt,
        read_context: *mut c_void,
        size: u64,
        block: u32,
        blocks: u32,
        cancel: CancelCallback,
        cancel_context: *mut c_void,
        handle_out: *mut *mut c_void,
        list: *mut NativeList,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn lf_preview_extract(
        handle: *mut c_void,
        index: u32,
        max_bytes: u64,
        cancel: CancelCallback,
        cancel_context: *mut c_void,
        image: *mut NativeImage,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn lf_preview_release(handle: *mut c_void);
    fn lf_preview_close(handle: *mut c_void);
}

/// The caller's source as the native stream reads it: the reader, what has been fetched against
/// the budget, and why reading stopped, if it did.
struct Source<R> {
    reader: R,
    budget: u64,
    fetched: Cell<u64>,
    failure: RefCell<Option<RawError>>,
}

impl<R: RandomAccess> Source<R> {
    /// Fill `dest` from `offset`, within the budget. The budget is taken before reading, so the
    /// reader is never asked for bytes past it.
    fn fetch(&self, offset: u64, dest: &mut [u8]) -> Result<(), RawError> {
        let length = dest.len() as u64;
        if self.fetched.get().saturating_add(length) > self.budget {
            return Err(RawError::ResourceLimit(
                "embedded-preview read budget exhausted",
            ));
        }
        let mut filled = 0;
        while filled < dest.len() {
            let at = offset
                .checked_add(filled as u64)
                .ok_or(RawError::InvalidInput("source offset overflow"))?;
            match self.reader.read_at(at, &mut dest[filled..]) {
                Ok(0) => {
                    return Err(RawError::Io {
                        kind: io::ErrorKind::UnexpectedEof,
                        message: format!(
                            "the source ended at byte {at}, before its length at open"
                        ),
                    });
                }
                Ok(n) => {
                    let n = n.min(dest.len() - filled);
                    self.fetched.set(self.fetched.get() + n as u64);
                    filled += n;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(read_error(&error)),
            }
        }
        Ok(())
    }

    /// Keep the first reason reading stopped.
    fn record(&self, failure: RawError) {
        if let Ok(mut slot) = self.failure.try_borrow_mut() {
            slot.get_or_insert(failure);
        }
    }

    fn failure(&self) -> Option<RawError> {
        self.failure.try_borrow().ok().and_then(|slot| slot.clone())
    }
}

fn read_error(error: &io::Error) -> RawError {
    RawError::Io {
        kind: error.kind(),
        message: error.to_string(),
    }
}

/// The native stream's read callback: fill `length` bytes at `offset` from the source `context`
/// points at. Never unwinds into C++: a panicking reader is caught and reported as a failure.
extern "C" fn read_at<R: RandomAccess>(
    context: *mut c_void,
    offset: u64,
    dest: *mut u8,
    length: usize,
) -> c_int {
    // SAFETY: `context` is the Source the handle owns, which outlives the native handle whose
    // stream calls this, and it is only read through shared references (its mutable parts are
    // cells). The call is synchronous, on the thread running the native call.
    let source = unsafe { &*context.cast::<Source<R>>() };
    if source.failure().is_some() {
        return 1;
    }
    if length == 0 {
        return 0;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the stream passes a buffer of `length` writable bytes that nothing else
        // touches until this call returns.
        let dest = unsafe { std::slice::from_raw_parts_mut(dest, length) };
        source.fetch(offset, dest)
    }));
    match outcome {
        Ok(Ok(())) => 0,
        Ok(Err(failure)) => {
            source.record(failure);
            1
        }
        Err(_) => {
            source.record(RawError::Native("source read: the reader panicked".into()));
            1
        }
    }
}

/// A RAW file's embedded images, listed at open and extracted on request, never unpacking its
/// mosaic. One handle serves one source on one thread at a time: every native call takes
/// `&mut self`. Dropping it closes the native handle, then frees the reader.
pub struct EmbeddedPreviews<R: RandomAccess> {
    /// The native handle, null only while opening; closed exactly once, by `Drop`.
    handle: *mut c_void,
    /// The reader the native stream calls back into, allocated by `open` and freed by `Drop`
    /// after the handle, so it outlives every native call that can reach it.
    source: NonNull<Source<R>>,
    listing: PreviewListing,
}

// SAFETY: the native handle is a heap object with no thread affinity: LibRaw keeps its state in
// the object (its "TLS" is a per-object allocation), and the stream in the same allocation holds
// only the reader pointer and its blocks. Neither retains a cancel token or any pointer into a
// thread's stack between calls. The source is moved with the handle, so it needs `R: Send`. The
// handle is never shared (`!Sync`): every native call takes `&mut self`. The native test counter
// of live handles is per thread, so it is exact only for a handle closed where it opened.
unsafe impl<R: RandomAccess + Send> Send for EmbeddedPreviews<R> {}

impl<R: RandomAccess> Drop for EmbeddedPreviews<R> {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or the unique handle lf_preview_open returned; close
        // accepts null and is called exactly once, here, before the source it reads is freed.
        unsafe { lf_preview_close(self.handle) };
        // SAFETY: `source` came from Box::leak in `open` and is freed once, here, after the only
        // other pointer to it (the native stream's) is gone.
        drop(unsafe { Box::from_raw(self.source.as_ptr()) });
    }
}

/// Frees the image an extraction borrowed from LibRaw, on every path out of `extract`.
struct Release(*mut c_void);
impl Drop for Release {
    fn drop(&mut self) {
        // SAFETY: the live handle `extract` borrows mutably; release accepts a handle without an
        // image.
        unsafe { lf_preview_release(self.0) };
    }
}

impl<R: RandomAccess> EmbeddedPreviews<R> {
    /// Identify `source` through LibRaw, without unpacking it, and list its embedded images.
    /// Every byte fetched from the source, here and in later extractions, counts against
    /// `read_budget` ([`RawError::ResourceLimit`] past it); at most
    /// [`MAX_EMBEDDED_READ_BUDGET`] is accepted. `cancel` is checked before any work, before each
    /// block fetched and through LibRaw's progress callback.
    pub fn open(source: R, read_budget: u64, cancel: &AtomicBool) -> Result<Self, RawError> {
        if read_budget > MAX_EMBEDDED_READ_BUDGET {
            return Err(RawError::InvalidInput(
                "read budget above MAX_EMBEDDED_READ_BUDGET",
            ));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let size = source.size().map_err(|error| read_error(&error))?;
        if size == 0 {
            return Err(RawError::InvalidInput("empty source"));
        }
        let source = NonNull::from(Box::leak(Box::new(Source {
            reader: source,
            budget: read_budget,
            fetched: Cell::new(0),
            failure: RefCell::new(None),
        })));
        // Owns the source from here: dropped on every error path below, it frees it.
        let mut previews = Self {
            handle: std::ptr::null_mut(),
            source,
            listing: PreviewListing {
                make: String::new(),
                model: String::new(),
                width: 0,
                height: 0,
                raw_width: 0,
                raw_height: 0,
                orientation: 1,
                previews: Vec::new(),
            },
        };
        let mut list = NativeList::blank();
        let mut error = [0 as c_char; 256];
        // SAFETY: the source outlives the native handle (Drop closes the handle first); the
        // callback is instantiated for its type; list, handle and error are writable for the
        // synchronous call, and the cancel token lives for it and is not retained.
        let code = unsafe {
            lf_preview_open(
                read_at::<R>,
                source.as_ptr().cast(),
                size,
                READ_BLOCK,
                READ_BLOCKS,
                cancelled,
                token(cancel),
                &mut previews.handle,
                &mut list,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        previews.outcome(code, &error)?;
        previews.listing = listing(&list)?;
        Ok(previews)
    }

    /// What identify read and the embedded images it listed.
    pub fn listing(&self) -> &PreviewListing {
        &self.listing
    }

    /// Every byte fetched from the source so far, by identify, the listing and extractions: whole
    /// blocks for small reads, exact lengths for large ones.
    pub fn bytes_read(&self) -> u64 {
        self.source().fetched.get()
    }

    /// Extract the listed image at LibRaw list `index` (an [`EmbeddedPreview::index`]) with
    /// LibRaw's `unpack_thumb_ex`, as owned bytes: a JPEG exactly as stored, or 8-bit RGB.
    ///
    /// `max_bytes`, at most [`MAX_EMBEDDED_IMAGE_BYTES`], bounds LibRaw's allocation for the
    /// image, checked from the list item before LibRaw allocates and again on what it produced,
    /// and the copy returned: an extraction holds LibRaw's buffer and the copy at once, so at most
    /// twice `max_bytes`, and LibRaw's buffer is freed before this returns. A format the crate
    /// does not hand over is [`RawError::UnsupportedCompression`] naming it; an image past
    /// `max_bytes` or the read budget is [`RawError::ResourceLimit`]. After a cancellation or a
    /// failure while reading, the handle refuses further extractions.
    pub fn extract(
        &mut self,
        index: usize,
        max_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<EmbeddedImage, RawError> {
        if max_bytes > MAX_EMBEDDED_IMAGE_BYTES {
            return Err(RawError::InvalidInput(
                "byte limit above MAX_EMBEDDED_IMAGE_BYTES",
            ));
        }
        if let Some(failure) = self.source().failure() {
            return Err(failure);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let preview = *self
            .listing
            .previews
            .iter()
            .find(|preview| preview.index == index)
            .ok_or(RawError::InvalidInput("no embedded image at this index"))?;
        if !preview.format.extractable() {
            return Err(RawError::UnsupportedCompression(preview.format.refusal()));
        }
        let over = || RawError::ResourceLimit("embedded image exceeds the byte limit");
        if preview.format != PreviewFormat::Jpeg
            && rgb_len(preview.width, preview.height) > max_bytes
        {
            return Err(over());
        }
        let mut image = NativeImage {
            data: std::ptr::null(),
            length: 0,
            kind: 0,
            width: 0,
            height: 0,
            colors: 0,
        };
        let mut error = [0 as c_char; 256];
        // SAFETY: the handle is live and borrowed mutably, so no other call runs on it; image and
        // error are writable for the synchronous call, and the cancel token lives for it and is
        // not retained. The index fits: the list holds at most eight items.
        let code = unsafe {
            lf_preview_extract(
                self.handle,
                index as u32,
                max_bytes as u64,
                cancelled,
                token(cancel),
                &mut image,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let _release = Release(self.handle);
        self.outcome(code, &error)?;
        let length = usize::try_from(image.length)
            .ok()
            .filter(|&length| length <= max_bytes)
            .ok_or_else(over)?;
        if image.data.is_null() {
            return Err(RawError::Native("LibRaw returned no embedded image".into()));
        }
        // SAFETY: on success `data` points to `length` bytes of LibRaw's thumbnail buffer, which
        // stays allocated until `_release` drops; nothing else runs on the handle meanwhile.
        let stored = unsafe { std::slice::from_raw_parts(image.data, length) };
        match image.kind {
            NATIVE_JPEG => {
                if !stored.starts_with(&[0xff, 0xd8]) {
                    return Err(RawError::Native(
                        "embedded JPEG does not begin with SOI".into(),
                    ));
                }
                Ok(EmbeddedImage::Jpeg(owned(stored)?))
            }
            NATIVE_BITMAP => {
                let pixels = image.width as usize * image.height as usize;
                let rgb = rgb_len(image.width, image.height);
                if rgb > max_bytes {
                    return Err(over());
                }
                let pixels = match image.colors {
                    3 if stored.len() == rgb => owned(stored)?,
                    1 if stored.len() == pixels => {
                        let mut expanded = Vec::new();
                        expanded
                            .try_reserve_exact(rgb)
                            .map_err(|_| RawError::ResourceLimit("embedded image allocation"))?;
                        expanded.extend(stored.iter().flat_map(|&value| [value; 3]));
                        expanded
                    }
                    _ => {
                        return Err(RawError::Native(
                            "LibRaw returned a bitmap whose length is not its pixels".into(),
                        ));
                    }
                };
                Ok(EmbeddedImage::Rgb8 {
                    width: image.width,
                    height: image.height,
                    pixels,
                })
            }
            _ => Err(RawError::Native(
                "LibRaw returned an unknown embedded image kind".into(),
            )),
        }
    }

    fn source(&self) -> &Source<R> {
        // SAFETY: the source is allocated for the handle's whole life and only read through
        // shared references.
        unsafe { self.source.as_ref() }
    }

    /// The outcome of a native call: why the source stopped, if it did, whatever code the call
    /// returned; otherwise the code's own error. In these calls `Geometry` is the byte limit.
    fn outcome(&self, code: c_int, error: &[c_char]) -> Result<(), RawError> {
        if let Some(failure) = self.source().failure() {
            return Err(failure);
        }
        if code == NativeStatus::Geometry as c_int {
            return Err(RawError::ResourceLimit(
                "embedded image exceeds the byte limit",
            ));
        }
        native_result(code, error)
    }
}

/// Bytes of 8-bit RGB for `width` × `height`.
fn rgb_len(width: u32, height: u32) -> usize {
    3 * width as usize * height as usize
}

/// A fallible owned copy.
fn owned(bytes: &[u8]) -> Result<Vec<u8>, RawError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| RawError::ResourceLimit("embedded image allocation"))?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

/// The typed listing of a native list.
fn listing(list: &NativeList) -> Result<PreviewListing, RawError> {
    let count = (list.count as usize).min(list.items.len());
    Ok(PreviewListing {
        make: c_text(&list.make),
        model: c_text(&list.model),
        width: list.width,
        height: list.height,
        raw_width: list.raw_width,
        raw_height: list.raw_height,
        orientation: exif_orientation(list.flip)?,
        previews: list.items[..count]
            .iter()
            .enumerate()
            .filter(|(_, item)| item.offset != 0 || item.length != 0)
            .map(|(index, item)| EmbeddedPreview {
                index,
                format: PreviewFormat::of(item),
                width: item.width,
                height: item.height,
                bytes: u64::from(item.length),
                orientation: exif_orientation(item.flip).ok(),
                offset: item.offset,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    unsafe extern "C" {
        fn lf_preview_live_handles() -> std::ffi::c_long;
        fn lf_preview_raw_allocated(handle: *mut c_void) -> c_int;
        fn lf_preview_list_buffer(
            bytes: *const u8,
            length: usize,
            list: *mut NativeList,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
        fn lf_preview_stream_matches_buffer(
            bytes: *const u8,
            length: usize,
            block: u32,
            blocks: u32,
            seed: u32,
            operations: u32,
            err: *mut c_char,
            err_len: usize,
        ) -> c_int;
    }

    /// Preview handles alive on this thread.
    fn live_handles() -> i64 {
        // c_long is 32 bits on Windows, so this conversion is not a no-op everywhere.
        #[allow(clippy::useless_conversion)]
        // SAFETY: reads this thread's counter; no arguments.
        i64::from(unsafe { lf_preview_live_handles() })
    }

    fn raw_allocated<R: RandomAccess>(previews: &EmbeddedPreviews<R>) -> bool {
        // SAFETY: the handle is live; the call only reads LibRaw's raw-data pointers.
        unsafe { lf_preview_raw_allocated(previews.handle) != 0 }
    }

    /// The listing LibRaw's own buffer datastream gives for `bytes`.
    pub(crate) fn buffer_listing(bytes: &[u8]) -> Result<PreviewListing, RawError> {
        let mut list = NativeList::blank();
        let mut error = [0 as c_char; 256];
        // SAFETY: bytes, list and error are valid for the synchronous call.
        let code = unsafe {
            lf_preview_list_buffer(
                bytes.as_ptr(),
                bytes.len(),
                &mut list,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(code, &error)?;
        listing(&list)
    }

    const LIMIT: usize = MAX_EMBEDDED_IMAGE_BYTES;
    const BUDGET: u64 = MAX_EMBEDDED_READ_BUDGET;

    /// A 48 × 32 baseline JPEG (811 bytes) from the JPEG fixtures.
    const JPEG: &[u8] = include_bytes!("../../../fixtures/s0/jpeg-scan-444-interleaved.jpg");

    /// One IFD entry: tag, type, count and its value's bytes.
    type Entry = (u16, u16, u32, Vec<u8>);

    fn long(tag: u16, value: u32) -> Entry {
        (tag, 4, 1, value.to_le_bytes().to_vec())
    }
    fn short(tag: u16, value: u16) -> Entry {
        (tag, 3, 1, value.to_le_bytes().to_vec())
    }
    fn text(tag: u16, value: &str) -> Entry {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        (tag, 2, bytes.len() as u32, bytes)
    }

    /// A little-endian IFD at `at` with its out-of-line values after it, and no next IFD.
    fn ifd(at: usize, mut entries: Vec<Entry>) -> Vec<u8> {
        entries.sort_by_key(|entry| entry.0);
        let mut payload = at + 2 + entries.len() * 12 + 4;
        let mut out = (entries.len() as u16).to_le_bytes().to_vec();
        let mut tail = Vec::new();
        for (tag, kind, count, value) in &entries {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            if value.len() <= 4 {
                let mut inline = [0_u8; 4];
                inline[..value.len()].copy_from_slice(value);
                out.extend_from_slice(&inline);
            } else {
                out.extend_from_slice(&(payload as u32).to_le_bytes());
                tail.extend_from_slice(value);
                if value.len() % 2 == 1 {
                    tail.push(0);
                }
                payload = at + 2 + entries.len() * 12 + 4 + tail.len();
            }
        }
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&tail);
        out
    }

    /// A DNG laid out as cameras write one: IFD0 holds `preview` (a JPEG, NewSubFileType 1)
    /// with the make, model and DNG version, and its one SubIFD a 16-bit RGGB mosaic of
    /// `width` × `height`. The preview is stored last, past the mosaic, so reading it needs a
    /// fetch of its own. Returns the file and the preview's offset.
    fn dng_with_preview(
        make: &str,
        model: &str,
        preview: &[u8],
        preview_size: (u32, u32),
        width: u32,
        height: u32,
    ) -> (Vec<u8>, usize) {
        let ifd0_entries = |sub: u32, jpeg: u32| {
            vec![
                long(254, 1),
                long(256, preview_size.0),
                long(257, preview_size.1),
                (
                    258,
                    3,
                    3,
                    [8_u16, 8, 8].iter().flat_map(|v| v.to_le_bytes()).collect(),
                ),
                short(259, 7),
                short(262, 6),
                text(271, make),
                text(272, model),
                long(273, jpeg),
                short(277, 3),
                long(278, preview_size.1),
                long(279, preview.len() as u32),
                short(284, 1),
                long(330, sub),
                (50_706, 1, 4, vec![1, 4, 0, 0]),
            ]
        };
        let raw_entries = |data: u32| {
            vec![
                long(254, 0),
                long(256, width),
                long(257, height),
                short(258, 16),
                short(259, 1),
                short(262, 32_803),
                long(273, data),
                short(277, 1),
                long(278, height),
                long(279, width * height * 2),
                short(284, 1),
                (33_421, 3, 2, vec![2, 0, 2, 0]),
                (33_422, 1, 4, vec![0, 1, 1, 2]),
                long(50_717, 65_535),
            ]
        };
        // Sizes do not depend on the offsets, so lay out once with zeros, then for real.
        let ifd0_len = ifd(8, ifd0_entries(0, 0)).len();
        let sub_at = (8 + ifd0_len).next_multiple_of(2);
        let sub_len = ifd(sub_at, raw_entries(0)).len();
        let raw_at = (sub_at + sub_len).next_multiple_of(2);
        let jpeg_at = raw_at + (width * height * 2) as usize;
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        out.extend(ifd(8, ifd0_entries(sub_at as u32, jpeg_at as u32)));
        out.resize(sub_at, 0);
        out.extend(ifd(sub_at, raw_entries(raw_at as u32)));
        out.resize(raw_at, 0);
        for i in 0..width * height {
            out.extend_from_slice(&((i * 37 % 60_000) as u16).to_le_bytes());
        }
        out.extend_from_slice(preview);
        (out, jpeg_at)
    }

    /// A 1.5 MiB DNG whose preview is 811 bytes.
    fn sample() -> (Vec<u8>, usize) {
        dng_with_preview("Example", "Sensor", JPEG, (48, 32), 1024, 768)
    }

    fn jpeg_preview(listing: &PreviewListing) -> EmbeddedPreview {
        *listing
            .previews
            .iter()
            .find(|preview| preview.format == PreviewFormat::Jpeg)
            .expect("a JPEG preview")
    }

    /// A reader that counts its calls and can fail, panic or cancel at a given call.
    struct Scripted<'a> {
        bytes: &'a [u8],
        calls: AtomicUsize,
        at: usize,
        action: Action<'a>,
    }
    enum Action<'a> {
        None,
        Fail,
        Panic,
        Cancel(&'a AtomicBool),
    }
    impl<'a> Scripted<'a> {
        fn new(bytes: &'a [u8], at: usize, action: Action<'a>) -> Self {
            Self {
                bytes,
                calls: AtomicUsize::new(0),
                at,
                action,
            }
        }
    }
    impl RandomAccess for Scripted<'_> {
        fn size(&self) -> io::Result<u64> {
            self.bytes.size()
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call >= self.at {
                match self.action {
                    Action::None => {}
                    Action::Fail => return Err(io::Error::other("card removed")),
                    Action::Panic => panic!("reader panicked on purpose"),
                    Action::Cancel(cancel) => cancel.store(true, Ordering::Relaxed),
                }
            }
            self.bytes.read_at(offset, buf)
        }
    }

    /// The synthetic DNG lists its JPEG preview and identify's view of the file, and the
    /// extraction returns exactly the stored bytes, having read a small part of the file.
    #[test]
    fn embedded_synthetic_dng_lists_and_extracts_its_jpeg() {
        let (bytes, jpeg_at) = sample();
        let cancel = AtomicBool::new(false);
        let mut previews = EmbeddedPreviews::open(&bytes[..], BUDGET, &cancel).unwrap();
        let listing = previews.listing().clone();
        assert_eq!(
            (listing.make.as_str(), listing.model.as_str()),
            ("Example", "Sensor")
        );
        assert_eq!((listing.raw_width, listing.raw_height), (1024, 768));
        assert_eq!((listing.width, listing.height), (1024, 768));
        assert_eq!(listing.orientation, 1);
        let preview = jpeg_preview(&listing);
        assert_eq!(
            (preview.width, preview.height, preview.bytes, preview.offset),
            (48, 32, JPEG.len() as u64, jpeg_at as u64)
        );
        assert_eq!(preview.orientation, Some(1));
        let opened = previews.bytes_read();
        assert!(opened > 0 && opened < bytes.len() as u64 / 8, "{opened}");
        let image = previews.extract(preview.index, LIMIT, &cancel).unwrap();
        assert_eq!(image, EmbeddedImage::Jpeg(JPEG.to_vec()));
        // Extracting again reads the same bytes and returns the same image.
        assert_eq!(
            previews
                .extract(preview.index, JPEG.len(), &cancel)
                .unwrap(),
            image
        );
        assert!(previews.bytes_read() < bytes.len() as u64 / 8);
        assert!(!raw_allocated(&previews), "never unpacked");
        assert_eq!(live_handles(), 1);
        drop(previews);
        assert_eq!(live_handles(), 0);
        // The native stream lists exactly what LibRaw's own buffer stream lists.
        assert_eq!(buffer_listing(&bytes).unwrap(), listing);
    }

    /// A file and the same bytes in memory are one path: equal listings, images and reads.
    #[test]
    fn embedded_file_and_memory_sources_agree() {
        let (bytes, _) = sample();
        let path = luxforge_testbase::paths::temp_path("embedded.dng");
        std::fs::write(&path, &bytes).unwrap();
        let cancel = AtomicBool::new(false);
        let mut from_file =
            EmbeddedPreviews::open(File::open(&path).unwrap(), BUDGET, &cancel).unwrap();
        let mut from_memory = EmbeddedPreviews::open(bytes.clone(), BUDGET, &cancel).unwrap();
        assert_eq!(from_file.listing(), from_memory.listing());
        let index = jpeg_preview(from_file.listing()).index;
        assert_eq!(
            from_file.extract(index, LIMIT, &cancel).unwrap(),
            from_memory.extract(index, LIMIT, &cancel).unwrap()
        );
        assert_eq!(from_file.bytes_read(), from_memory.bytes_read());
        drop(from_file);
        std::fs::remove_file(&path).unwrap();
        // An Arc of the bytes, as a caller holding a bounded read would pass it.
        let shared: Arc<[u8]> = Arc::from(bytes);
        let shared = EmbeddedPreviews::open(shared, BUDGET, &cancel).unwrap();
        assert_eq!(shared.listing(), from_memory.listing());
    }

    /// Empty, garbage and truncated sources are typed refusals, never panics, and release every
    /// handle.
    #[test]
    fn embedded_empty_garbage_and_truncated_sources_are_refused() {
        let cancel = AtomicBool::new(false);
        assert_eq!(
            EmbeddedPreviews::open(Vec::new(), BUDGET, &cancel).err(),
            Some(RawError::InvalidInput("empty source"))
        );
        let garbage: Vec<u8> = (0..70_000_u32).map(|i| (i * 7919 % 251) as u8).collect();
        assert!(matches!(
            EmbeddedPreviews::open(&garbage[..], BUDGET, &cancel),
            Err(RawError::Native(_))
        ));
        assert!(matches!(
            EmbeddedPreviews::open(&b"II*\0"[..], BUDGET, &cancel),
            Err(RawError::Native(_))
        ));
        let (bytes, jpeg_at) = dng_with_preview("Example", "Sensor", JPEG, (48, 32), 32, 24);
        // Every prefix of the file: refused at open, or listed and then refused or extracted
        // whole, never more than was there.
        let mut listed = 0;
        for cut in (1..=bytes.len()).step_by(7) {
            let prefix = &bytes[..cut];
            match EmbeddedPreviews::open(prefix, BUDGET, &cancel) {
                Err(error) => assert!(
                    matches!(error, RawError::Native(_) | RawError::InvalidInput(_)),
                    "{cut}: {error:?}"
                ),
                Ok(mut previews) => {
                    listed += 1;
                    for preview in previews.listing().previews.clone() {
                        match previews.extract(preview.index, LIMIT, &cancel) {
                            Ok(EmbeddedImage::Jpeg(image)) => {
                                assert!(cut >= jpeg_at + JPEG.len(), "{cut}");
                                assert_eq!(image, JPEG);
                            }
                            Ok(image) => panic!("{cut}: {image:?}"),
                            Err(error) => assert!(
                                matches!(
                                    error,
                                    RawError::Native(_)
                                        | RawError::InvalidInput(_)
                                        | RawError::UnsupportedCompression(_)
                                ),
                                "{cut}: {error:?}"
                            ),
                        }
                    }
                }
            }
        }
        println!("{listed} prefixes listed");
        assert_eq!(live_handles(), 0);
    }

    /// Cancellation before work, during identify's reads and during an extraction's reads stops
    /// with `Cancelled`; a handle whose extraction was cancelled refuses further extractions.
    #[test]
    fn embedded_cancellation_before_and_during_reads() {
        let (bytes, _) = sample();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            EmbeddedPreviews::open(&bytes[..], BUDGET, &cancel).err(),
            Some(RawError::Cancelled)
        );
        // Identify's fetches: cancelled at each of the first few.
        let open_calls = {
            let reader = Scripted::new(&bytes, usize::MAX, Action::None);
            let calm = AtomicBool::new(false);
            let previews = EmbeddedPreviews::open(&reader, BUDGET, &calm).unwrap();
            drop(previews);
            reader.calls.load(Ordering::Relaxed)
        };
        assert!(open_calls >= 1);
        for at in 1..=open_calls {
            let cancel = AtomicBool::new(false);
            let reader = Scripted::new(&bytes, at, Action::Cancel(&cancel));
            assert_eq!(
                EmbeddedPreviews::open(&reader, BUDGET, &cancel).err(),
                Some(RawError::Cancelled),
                "fetch {at}"
            );
        }
        // An extraction's reads.
        let cancel = AtomicBool::new(false);
        let reader = Scripted::new(&bytes, open_calls + 1, Action::Cancel(&cancel));
        let mut previews = EmbeddedPreviews::open(&reader, BUDGET, &cancel).unwrap();
        let index = jpeg_preview(previews.listing()).index;
        assert_eq!(
            previews.extract(index, LIMIT, &cancel),
            Err(RawError::Cancelled)
        );
        cancel.store(false, Ordering::Relaxed);
        let spent = previews.extract(index, LIMIT, &cancel);
        assert!(
            matches!(&spent, Err(RawError::Native(text)) if text.contains("open the source again")),
            "{spent:?}"
        );
        drop(previews);
        // Cancelled before the extraction's work.
        let cancel = AtomicBool::new(false);
        let mut previews = EmbeddedPreviews::open(&bytes[..], BUDGET, &cancel).unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            previews.extract(index, LIMIT, &cancel),
            Err(RawError::Cancelled)
        );
        cancel.store(false, Ordering::Relaxed);
        assert!(previews.extract(index, LIMIT, &cancel).is_ok());
        drop(previews);
        assert_eq!(live_handles(), 0);
    }

    /// The read budget bounds every fetch: identify with less than it needs is refused, the
    /// bytes read never pass it, and an extraction past what is left is refused.
    #[test]
    fn embedded_read_budget_bounds_every_fetch() {
        let (bytes, _) = sample();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            EmbeddedPreviews::open(&bytes[..], BUDGET + 1, &cancel).err(),
            Some(RawError::InvalidInput(
                "read budget above MAX_EMBEDDED_READ_BUDGET"
            ))
        );
        let budget_error = Some(RawError::ResourceLimit(
            "embedded-preview read budget exhausted",
        ));
        assert_eq!(
            EmbeddedPreviews::open(&bytes[..], 1024, &cancel).err(),
            budget_error
        );
        let needed = EmbeddedPreviews::open(&bytes[..], BUDGET, &cancel)
            .unwrap()
            .bytes_read();
        assert_eq!(
            EmbeddedPreviews::open(&bytes[..], needed - 1, &cancel).err(),
            budget_error
        );
        let mut previews = EmbeddedPreviews::open(&bytes[..], needed, &cancel).unwrap();
        assert_eq!(previews.bytes_read(), needed);
        let index = jpeg_preview(previews.listing()).index;
        // The preview, stored past the mosaic, needs fetches of its own.
        assert_eq!(previews.extract(index, LIMIT, &cancel).err(), budget_error);
        assert_eq!(previews.bytes_read(), needed);
        // The extraction's SOI check and one block, at most.
        let mut previews =
            EmbeddedPreviews::open(&bytes[..], needed + 2 + u64::from(READ_BLOCK), &cancel)
                .unwrap();
        assert_eq!(
            previews.extract(index, LIMIT, &cancel).unwrap(),
            EmbeddedImage::Jpeg(JPEG.to_vec())
        );
        println!(
            "open and list {needed} bytes, extract {} more, of {}",
            previews.bytes_read() - needed,
            bytes.len()
        );
    }

    /// A byte limit below the item and an index outside the list are refused before any read,
    /// and leave the handle usable.
    #[test]
    fn embedded_byte_limit_and_index_are_checked_first() {
        let (bytes, _) = sample();
        let cancel = AtomicBool::new(false);
        let mut previews = EmbeddedPreviews::open(&bytes[..], BUDGET, &cancel).unwrap();
        let index = jpeg_preview(previews.listing()).index;
        let read = previews.bytes_read();
        assert_eq!(
            previews.extract(index, JPEG.len() - 1, &cancel),
            Err(RawError::ResourceLimit(
                "embedded image exceeds the byte limit"
            ))
        );
        for missing in [8, 99, usize::MAX] {
            assert_eq!(
                previews.extract(missing, LIMIT, &cancel),
                Err(RawError::InvalidInput("no embedded image at this index"))
            );
        }
        assert_eq!(
            previews.extract(index, LIMIT + 1, &cancel),
            Err(RawError::InvalidInput(
                "byte limit above MAX_EMBEDDED_IMAGE_BYTES"
            ))
        );
        assert_eq!(previews.bytes_read(), read);
        assert_eq!(
            previews.extract(index, JPEG.len(), &cancel).unwrap(),
            EmbeddedImage::Jpeg(JPEG.to_vec())
        );
    }

    /// A reader's error and a reader's panic end the call with a typed error, never an unwind
    /// through C++, and the handle is released.
    #[test]
    fn embedded_reader_failures_are_typed() {
        let (bytes, _) = sample();
        let cancel = AtomicBool::new(false);
        let reader = Scripted::new(&bytes, 1, Action::Fail);
        assert!(
            matches!(
                EmbeddedPreviews::open(&reader, BUDGET, &cancel).err(),
                Some(RawError::Io { kind: io::ErrorKind::Other, message }) if message == "card removed"
            ),
            "a reader's I/O error is the source's, not a corrupt file's"
        );
        let reader = Scripted::new(&bytes, 1, Action::Panic);
        assert_eq!(
            EmbeddedPreviews::open(&reader, BUDGET, &cancel).err(),
            Some(RawError::Native("source read: the reader panicked".into()))
        );
        // A source that ends before the length it gave.
        struct Shrunk<'a>(&'a [u8], u64);
        impl RandomAccess for Shrunk<'_> {
            fn size(&self) -> io::Result<u64> {
                Ok(self.1)
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
                self.0.read_at(offset, buf)
            }
        }
        let error =
            EmbeddedPreviews::open(Shrunk(&bytes[..1000], bytes.len() as u64), BUDGET, &cancel)
                .err();
        assert!(
            matches!(
                &error,
                Some(RawError::Io { kind: io::ErrorKind::UnexpectedEof, message })
                    if message.contains("ended at byte")
            ),
            "{error:?}"
        );
        assert_eq!(live_handles(), 0);
    }

    /// A preview LibRaw declares a JPEG is listed by what its bytes are: SOI makes a JPEG,
    /// Canon's H.265 header an H.265, anything else not a JPEG, and neither is extracted.
    #[test]
    fn embedded_declared_jpegs_are_listed_by_their_bytes() {
        let cancel = AtomicBool::new(false);
        let mut h265 = vec![0, 0, 0, 1];
        h265.extend_from_slice(b"CISZ");
        h265.resize(JPEG.len(), 7);
        let mut deflate = vec![0x78, 0x9c];
        deflate.resize(JPEG.len(), 3);
        for (stored, format) in [
            (h265, PreviewFormat::H265),
            (deflate, PreviewFormat::NotJpeg),
        ] {
            let (bytes, _) = dng_with_preview("Example", "Sensor", &stored, (48, 32), 256, 192);
            let mut previews = EmbeddedPreviews::open(&bytes[..], BUDGET, &cancel).unwrap();
            let preview = *previews
                .listing()
                .previews
                .iter()
                .find(|preview| preview.bytes == stored.len() as u64)
                .unwrap();
            assert_eq!(preview.format, format);
            assert!(!format.extractable());
            assert_eq!(
                previews.extract(preview.index, LIMIT, &cancel),
                Err(RawError::UnsupportedCompression(format.refusal()))
            );
            assert_eq!(buffer_listing(&bytes).unwrap(), *previews.listing());
        }
    }

    /// The largest JPEG is the longest, whatever dimensions LibRaw lists, and never a format
    /// that is not a JPEG.
    #[test]
    fn embedded_largest_jpeg_is_the_longest() {
        let preview = |index, format, width, height, bytes| EmbeddedPreview {
            index,
            format,
            width,
            height,
            bytes,
            orientation: None,
            offset: 1,
        };
        let mut listing = PreviewListing {
            make: String::new(),
            model: String::new(),
            width: 6000,
            height: 4000,
            raw_width: 6000,
            raw_height: 4000,
            orientation: 1,
            previews: vec![
                preview(0, PreviewFormat::Jpeg, 160, 120, 16_000),
                preview(1, PreviewFormat::Jpeg, 0, 0, 3_000_000),
                preview(2, PreviewFormat::Jpeg, 1620, 1080, 400_000),
                preview(3, PreviewFormat::H265, 6000, 4000, 9_000_000),
            ],
        };
        assert_eq!(listing.largest_jpeg().map(|preview| preview.index), Some(1));
        listing
            .previews
            .retain(|preview| preview.format != PreviewFormat::Jpeg);
        assert_eq!(listing.largest_jpeg(), None);
    }

    /// The native stream answers every call as LibRaw's buffer datastream does, over reads,
    /// seeks, characters, lines and tokens that cross block boundaries and the end of the data.
    #[test]
    fn embedded_stream_calls_match_libraw_buffer_stream() {
        let mut state = 7_u32;
        let bytes: Vec<u8> = (0..20_011)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                match (state >> 16) % 16 {
                    0 => b'\n',
                    1 | 2 => b' ',
                    3..=8 => b'0' + ((state >> 8) % 10) as u8,
                    9 => b'-',
                    _ => (state >> 8) as u8,
                }
            })
            .collect();
        for (block, blocks, seed) in [(4096, 1, 1), (4096, 3, 2), (8192, 2, 3), (16_384, 8, 4)] {
            let mut error = [0 as c_char; 256];
            // SAFETY: bytes and error are valid for the synchronous call.
            let code = unsafe {
                lf_preview_stream_matches_buffer(
                    bytes.as_ptr(),
                    bytes.len(),
                    block,
                    blocks,
                    seed,
                    20_000,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert_eq!(code, 0, "{blocks} × {block}: {}", c_text(&error));
        }
    }

    /// Every RAW file in the directories LUXFORGE_PREVIEW_DIRS lists (a path list; each file
    /// once, whatever its name or link), read from its file through the native stream, lists
    /// exactly what LibRaw's own buffer datastream lists for the whole file in memory, or both
    /// refuse it; no listing leaves a raw image allocated. The files are only read.
    #[test]
    #[ignore = "requires the local RAW corpus"]
    fn embedded_corpus_lists_as_libraw_buffer_does() {
        let dirs = std::env::var("LUXFORGE_PREVIEW_DIRS").expect("LUXFORGE_PREVIEW_DIRS");
        let mut files = std::collections::BTreeSet::new();
        for dir in std::env::split_paths(&dirs) {
            for entry in std::fs::read_dir(dir).expect("list corpus directory") {
                let path = entry.unwrap().path();
                let data = path.extension().is_some_and(|extension| {
                    matches!(
                        extension.to_string_lossy().as_ref(),
                        "json" | "tsv" | "txt" | "md"
                    )
                });
                let hidden = path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'));
                if !data && !hidden {
                    files.insert(std::fs::canonicalize(&path).unwrap());
                }
            }
        }
        let cancel = AtomicBool::new(false);
        let (mut listed, mut refused) = (0, 0);
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            let expected = buffer_listing(&bytes);
            match EmbeddedPreviews::open(File::open(path).unwrap(), BUDGET, &cancel) {
                Ok(previews) => {
                    assert!(!raw_allocated(&previews), "{path:?}");
                    assert_eq!(expected.as_ref(), Ok(previews.listing()), "{path:?}");
                    listed += 1;
                }
                Err(error) => {
                    assert!(expected.is_err(), "{path:?}: {error}");
                    refused += 1;
                }
            }
        }
        assert_eq!(live_handles(), 0);
        println!(
            "{listed} files list as LibRaw's buffer stream lists them; {refused} refused by both"
        );
    }
}
