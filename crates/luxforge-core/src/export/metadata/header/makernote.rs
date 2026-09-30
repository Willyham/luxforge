//! The plain parts of maker notes: body serials that are stored as text or a number, and the small
//! JPEGs some makers keep there. Each maker's layout is recognized by its header, or for Canon,
//! which has none, by the make; enciphered notes (Sony's) and anything else are not read.
//!
//! | Maker | Header | Offsets relative to | Read |
//! | --- | --- | --- | --- |
//! | Nikon | `Nikon\0` + version, then a TIFF | that TIFF | 0x001d serial; 0x0011 preview IFD's JPEG |
//! | Canon | none (an IFD) | the enclosing TIFF | 0x000c serial, a number or text |
//! | Fujifilm | `FUJIFILM` + IFD offset, little-endian | the maker note | 0x0010 serial |
//! | Olympus | `OLYMPUS\0` + byte order + version | the maker note | 0x2010 Equipment's 0x0101 serial; 0x0100 thumbnail |
//! | OM System | `OM SYSTEM\0\0\0` + byte order + version | the maker note | as Olympus |
//! | Pentax | `PENTAX \0` + byte order | the maker note | 0x0229 serial |
//! | Pentax, Ricoh DNG private data (0xc634) | `PENTAX \0` or `RICOH\0` + byte order | the maker note | 0x0229 serial |
//! | Panasonic | `Panasonic\0\0\0` | the enclosing TIFF | 0x0025 serial |

use super::fields::{Found, SerialSource};
use super::source::Source;
use super::tiff::{Entry, Magic, Tiff};

/// Where the blob `entry` names starts (relative to `tiff` and absolute) and its first 16 bytes,
/// zero past its end.
fn note_head(source: &mut impl Source, tiff: &Tiff, entry: &Entry) -> Option<(u32, u64, [u8; 16])> {
    let (start, length) = tiff.blob(entry)?;
    let at = tiff.absolute(start);
    let mut head = [0; 16];
    let length = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .min(head.len());
    source
        .read(at, &mut head[..length])
        .then_some((start, at, head))
}

/// Read the maker note `entry` (the Exif IFD's 0x927c) of `tiff` into `found`.
pub(super) fn read(source: &mut impl Source, tiff: &Tiff, entry: &Entry, found: &mut Found) {
    let Some((start, at, head)) = note_head(source, tiff, entry) else {
        return;
    };
    let order = |at: usize| match &head[at..at + 2] {
        b"II" => Some(false),
        b"MM" => Some(true),
        _ => None,
    };
    if head.starts_with(b"Nikon\0") {
        nikon(source, tiff, at, found);
    } else if head.starts_with(b"FUJIFILM") {
        fujifilm(source, tiff, at, found);
    } else if head.starts_with(b"OLYMPUS\0") {
        if let Some(big_endian) = order(8) {
            olympus(
                source,
                Tiff::with_order(at, tiff.end(), big_endian),
                12,
                found,
            );
        }
    } else if head.starts_with(b"OM SYSTEM\0\0\0") {
        if let Some(big_endian) = order(12) {
            olympus(
                source,
                Tiff::with_order(at, tiff.end(), big_endian),
                16,
                found,
            );
        }
    } else if head.starts_with(b"PENTAX \0") {
        pentax(source, tiff, at, &head, found);
    } else if head.starts_with(b"Panasonic\0\0\0") {
        // Offsets relative to the enclosing TIFF; 12 bytes past the value's start.
        if let Some(ifd) = start.checked_add(12) {
            serial(source, tiff, ifd, 0x0025, found);
        }
    } else if found
        .make
        .as_deref()
        .is_some_and(|make| make.starts_with("Canon"))
    {
        canon(source, tiff, start, found);
    }
}

/// A DNG's private data (IFD0's 0xc634) when it is a Pentax or Ricoh maker note, as Pentax and
/// Ricoh DNGs keep it: its serial.
pub(super) fn dng_private(source: &mut impl Source, tiff: &Tiff, entry: &Entry, found: &mut Found) {
    if let Some((_, at, head)) = note_head(source, tiff, entry) {
        pentax(source, tiff, at, &head, found);
    }
}

/// A Pentax maker note, `PENTAX \0` and its byte order with the IFD 10 bytes in, or Ricoh's,
/// `RICOH\0` and its byte order with the IFD 8 bytes in; offsets from its start. Its serial
/// (0x0229).
fn pentax(source: &mut impl Source, tiff: &Tiff, at: u64, head: &[u8; 16], found: &mut Found) {
    let (order, ifd) = if head.starts_with(b"PENTAX \0") {
        (&head[8..10], 10)
    } else if head.starts_with(b"RICOH\0") {
        (&head[6..8], 8)
    } else {
        return;
    };
    let big_endian = match order {
        b"II" => false,
        b"MM" => true,
        _ => return,
    };
    serial(
        source,
        &Tiff::with_order(at, tiff.end(), big_endian),
        ifd,
        0x0229,
        found,
    );
}

/// A text serial `tag` in the IFD at `ifd`.
fn serial(source: &mut impl Source, note: &Tiff, ifd: u32, tag: u16, found: &mut Found) {
    let Some(entry) = note.ifd(source, ifd).and_then(|ifd| ifd.find(tag)) else {
        return;
    };
    found.serial(SerialSource::MakerNote, || note.text(source, &entry));
}

/// A Canon maker note's IFD at `ifd`: its serial (0x000c), a number (written as ten digits, as
/// Canon prints it) or text. Also a CR3's `CMT3`.
pub(super) fn canon(source: &mut impl Source, tiff: &Tiff, ifd: u32, found: &mut Found) {
    let Some(entry) = tiff.ifd(source, ifd).and_then(|ifd| ifd.find(0x000c)) else {
        return;
    };
    found.serial(SerialSource::MakerNote, || {
        tiff.text(source, &entry).or_else(|| {
            tiff.uint(source, &entry)
                .filter(|value| *value > 0)
                .map(|value| format!("{value:010}"))
        })
    });
}

/// A Nikon type-3 maker note: a TIFF 10 bytes in, with the serial and the preview IFD's JPEG.
fn nikon(source: &mut impl Source, tiff: &Tiff, at: u64, found: &mut Found) {
    let Some((note, ifd0)) = Tiff::open(source, at + 10, tiff.end(), Magic::Standard) else {
        return;
    };
    let Some(ifd) = note.ifd(source, ifd0) else {
        return;
    };
    if let Some(entry) = ifd.find(0x001d) {
        found.serial(SerialSource::MakerNote, || note.text(source, &entry));
    }
    let preview = ifd
        .find(0x0011)
        .and_then(|entry| note.uint(source, &entry))
        .and_then(|offset| note.ifd(source, offset));
    if let Some(preview) = preview {
        found.offer_jpeg_pointer(source, &note, &preview);
    }
}

/// A Fujifilm maker note: always little-endian, its IFD offset at 8, offsets from its start.
fn fujifilm(source: &mut impl Source, tiff: &Tiff, at: u64, found: &mut Found) {
    let Some(ifd) = source.array(at + 8).map(u32::from_le_bytes) else {
        return;
    };
    let note = Tiff::with_order(at, tiff.end(), false);
    serial(source, &note, ifd, 0x0010, found);
}

/// An Olympus or OM System maker note whose IFD is `ifd` bytes in: the Equipment IFD's serial and
/// the thumbnail JPEG.
fn olympus(source: &mut impl Source, note: Tiff, ifd: u32, found: &mut Found) {
    let Some(main) = note.ifd(source, ifd) else {
        return;
    };
    if let Some(equipment) = main
        .find(0x2010)
        .and_then(|entry| note.uint(source, &entry))
    {
        serial(source, &note, equipment, 0x0101, found);
    }
    if let Some((start, length)) = main.find(0x0100).and_then(|entry| note.blob(&entry)) {
        found.offer_jpeg(source, note.absolute(start), u64::from(length), None);
    }
}
