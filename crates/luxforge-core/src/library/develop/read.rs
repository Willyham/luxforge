//! Reading one pick for a Develop, on the lane's worker, off the owner and off the source worker:
//! one bounded read of the file that streams its SHA-256 (the fingerprint the catalog stores) as
//! it goes, with the file's signature before the first read and after the last; its header, from
//! the same bytes, through the index's header reader; and its interpretation, without developing
//! anything.
//!
//! - **A JPEG**'s interpretation is its size and orientation. Its header is read and checked as a
//!   decode checks it before its first row — the frame's kind, size, colour space and ICC profile
//!   — so a JPEG Luxforge cannot open is refused here, without decoding its scans: a full decode
//!   would cost a frame's allocation and tens to hundreds of milliseconds a file to find only a
//!   damaged entropy stream, which the first preparation of the photograph reports as it would for
//!   a file damaged after it was developed.
//! - **A RAW**'s interpretation is the RAW decoder's metadata, which `RawSource::decode` answers
//!   only after unpacking the sensor mosaic; the mosaic is dropped at once and nothing is
//!   demosaiced or developed.
//!
//! A card's pick may then be developed from a copy in an indexed folder ([`from_copy`]), once the
//! copy's own streamed fingerprint is the card file's.
use crate::{
    EditorService, Error, ErrorKind, SourceKind,
    atomic_file::file_error,
    catalog_types::{Dimensions, ExifOrientation, HeaderMetadata, PlaceNames, Volume},
    editor::{RawInterpretation, SourceSignature, source_signature, source_signature_for_handle},
    export::metadata::{header::read_header, jpeg_orientation},
    index::volume_of,
    jobs::JobControl,
    library::locate::{self, CHUNK, Phase},
    organize::Gazetteer,
    source::{JPEG_LIMITS, MAX_JPEG_BYTES, raw_error},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, ErrorKind as IoErrorKind, Read},
    path::{Path, PathBuf},
};

/// One file read and interpreted for a Develop: what its photograph is made of.
#[derive(Clone, Debug)]
pub(crate) struct ReadFile {
    /// Its canonical path, which the photograph's locator becomes.
    pub path: PathBuf,
    /// Its signature while it was read.
    pub signature: SourceSignature,
    /// The SHA-256 of its bytes, as 64 lowercase hex digits.
    pub fingerprint: String,
    pub source: SourceKind,
    /// Its upright size, as an asset records it.
    pub width: u32,
    pub height: u32,
    /// Its header, with its stored size and orientation from the interpretation when the header
    /// lacks them.
    pub header: HeaderMetadata,
    /// The place the gazetteer names at its position.
    pub place: Option<String>,
    /// The volume it is on, seen as it was read.
    pub volume: Volume,
}

/// Read, fingerprint and interpret the file at `path`, bounded by its kind's limit (JPEG
/// [`MAX_JPEG_BYTES`], anything else RAW's `MAX_SOURCE_BYTES`), in [`CHUNK`]s, cancellable
/// between them. A file that is missing is `source-unavailable`; one that changes while it is read,
/// `conflict`; one Luxforge cannot open, its decoder's refusal; a cancel, `cancelled`.
pub(crate) fn read(
    path: &Path,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<ReadFile, Error> {
    let cancelled = |error: Error| {
        if control.is_cancelled() {
            control.cancelled_error()
        } else {
            error
        }
    };
    let (path, signature, bytes, fingerprint) = read_bytes(path, control, pause)?;
    let header = read_header(&mut Cursor::new(&bytes[..]), bytes.len() as u64)
        .map(|read| read.header.metadata())
        .unwrap_or_default();
    let (source, stored, orientation) = if bytes.starts_with(&[0xff, 0xd8]) {
        jpeg(&bytes)?
    } else {
        // The bytes go to the decoder as read, and are dropped with the mosaic it unpacks.
        let sensor = luxforge_raw::RawSource::decode(bytes, control.flag())
            .map_err(|error| cancelled(raw_error(error)))?;
        let metadata = RawInterpretation::new(sensor.metadata().clone())?;
        drop(sensor);
        let crop = metadata.default_crop;
        let orientation = metadata.exif_orientation;
        (
            SourceKind::Raw { metadata },
            (crop.width, crop.height),
            orientation,
        )
    };
    control.checkpoint()?;
    let (width, height) = upright(stored, orientation);
    let header = HeaderMetadata {
        dimensions: header.dimensions.or(Some(Dimensions {
            width: stored.0,
            height: stored.1,
        })),
        orientation: header.orientation.or(ExifOrientation::new(orientation)),
        ..header
    };
    let place = header
        .position
        .as_ref()
        .and_then(|position| Gazetteer.nearest(position));
    let volume = volume_of(&path, crate::editor::now_ms())?;
    Ok(ReadFile {
        path,
        signature,
        fingerprint,
        source,
        width,
        height,
        header,
        place,
        volume,
    })
}

/// A card file's pick developed from a copy instead: the first of `copies` whose streamed
/// fingerprint is `card`'s, with its path, signature and volume, and everything else read from the
/// card's identical bytes; none when no copy holds them. A copy that cannot be read or holds other
/// bytes is passed over; only a cancel stops the search.
pub(crate) fn from_copy(
    card: &ReadFile,
    copies: &[PathBuf],
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<Option<ReadFile>, Error> {
    for copy in copies {
        control.checkpoint()?;
        let Ok((path, signature)) = EditorService::request_signature(copy) else {
            continue;
        };
        if signature.byte_len() != card.signature.byte_len() || path == card.path {
            continue;
        }
        match locate::verify_file(
            &path,
            &signature,
            &card.fingerprint,
            control,
            pause,
            &|_, _| {},
        ) {
            Ok(signature) => {
                let volume = volume_of(&path, crate::editor::now_ms())?;
                return Ok(Some(ReadFile {
                    path,
                    signature,
                    volume,
                    ..card.clone()
                }));
            }
            Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
            Err(_) => {}
        }
    }
    Ok(None)
}

/// The file's canonical path, its signature, its bytes and their SHA-256: one read from one open
/// handle, in [`CHUNK`]s, bounded by the file kind's limit once its first bytes say which it is.
pub(super) fn read_bytes(
    path: &Path,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<(PathBuf, SourceSignature, Vec<u8>, String), Error> {
    let unreadable = |error: std::io::Error| match error.kind() {
        IoErrorKind::NotFound => {
            Error::source_unavailable(format!("{} is missing", path.display()))
        }
        kind => file_error(format!("cannot read {}", path.display()), kind),
    };
    let canonical = path.canonicalize().map_err(unreadable)?;
    let mut file = File::open(&canonical).map_err(unreadable)?;
    let before = held_signature(&canonical, &file)?;
    if !file.metadata().map_err(unreadable)?.is_file() {
        return Err(Error::unsupported_input(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let len = before.byte_len();
    if len > luxforge_raw::MAX_SOURCE_BYTES as u64 {
        return Err(too_large(path));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len as usize)
        .map_err(|_| Error::resource_limit(format!("cannot hold {} to read it", path.display())))?;
    let mut hash = Sha256::new();
    loop {
        pause(Phase::Hashing);
        control.checkpoint()?;
        let from = bytes.len();
        let count = match file.by_ref().take(CHUNK as u64).read_to_end(&mut bytes) {
            Ok(count) => count,
            Err(error) if error.kind() == IoErrorKind::Interrupted => continue,
            Err(error) => return Err(unreadable(error)),
        };
        if count == 0 {
            break;
        }
        if bytes.len() as u64 > len {
            return Err(changed(path));
        }
        if from == 0 && bytes.starts_with(&[0xff, 0xd8]) && len > MAX_JPEG_BYTES as u64 {
            return Err(too_large(path));
        }
        hash.update(&bytes[from..]);
    }
    pause(Phase::Hashed);
    let after = held_signature(&canonical, &file)?;
    if after != before || bytes.len() as u64 != len {
        return Err(changed(path));
    }
    Ok((canonical, before, bytes, format!("{:x}", hash.finalize())))
}

/// A JPEG's interpretation from its header alone, checked as a decode checks it before its first
/// row: its stored size and EXIF orientation.
fn jpeg(bytes: &[u8]) -> Result<(SourceKind, (u32, u32), u8), Error> {
    let decoder = luxforge_jpeg::Decoder::new(bytes, JPEG_LIMITS)?;
    if let Some(profile) = decoder.icc_profile() {
        crate::profile::check(profile, decoder.components())?;
    }
    Ok((
        SourceKind::Jpeg,
        (decoder.width(), decoder.height()),
        jpeg_orientation(bytes),
    ))
}

/// A stored size turned upright by EXIF `orientation`: 5 to 8 swap the sides.
fn upright((width, height): (u32, u32), orientation: u8) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

/// The signature of the file open at `path`, which must be the same as the file at `path` now.
fn held_signature(path: &Path, file: &File) -> Result<SourceSignature, Error> {
    let unreadable =
        |error: std::io::Error| file_error(format!("cannot read {}", path.display()), error.kind());
    let held = file.metadata().map_err(unreadable)?;
    let at_path = path.metadata().map_err(unreadable)?;
    let signature = source_signature_for_handle(path, file, &held);
    if signature != source_signature(path, &at_path) {
        return Err(changed(path));
    }
    Ok(signature)
}

fn changed(path: &Path) -> Error {
    Error::conflict(format!("{} changed while it was read", path.display()))
}

fn too_large(path: &Path) -> Error {
    Error::resource_limit(format!(
        "{} is larger than an original Luxforge reads",
        path.display()
    ))
}
