//! The containers: which structures of each are read, in which order, and where the dimensions
//! and thumbnail come from. Every walk is bounded by counts of its own (segments, boxes, IFDs,
//! directory records), so the whole-file reader stops where the bounded one does.
//!
//! Dimensions, before orientation, as each container declares them:
//! - JPEG: the frame header (SOF).
//! - TIFF, ORF: the largest full-resolution raw IFD (NewSubfileType 0 or absent, and not a
//!   rendered preview) among IFD0, its SubIFDs and the IFD chain, cropped to its DefaultCropSize
//!   (DNG, 0xc620) or Sony's crop size (0x74c8) when it declares one; when no raw IFD declares a
//!   size (some CR2s), the Exif IFD's PixelXDimension and PixelYDimension. Crops kept in maker
//!   notes are not read, so NEF, CR2, PEF and ORF report the stored raw size, margins included.
//! - RW2: IFD0's sensor borders (0x0004–0x0007), the image area Panasonic declares; its sensor
//!   size (0x0002, 0x0003) when the borders are missing.
//! - RAF: the RAF directory's cropped size (0x0111), else its full size (0x0100), each stored
//!   height first.
//! - CR3: `CMT1`'s ImageWidth and ImageLength, the image size Canon declares; else `CMT2`'s
//!   PixelXDimension and PixelYDimension.

use super::fields::{Found, IfdKind};
use super::makernote;
use super::source::Source;
use super::tiff::{Ifd, Magic, Tiff};
use super::{Container, Dimensions, FileHeader};

// TIFF tags a walk follows.
const NEW_SUBFILE_TYPE: u16 = 0x00fe;
const IMAGE_WIDTH: u16 = 0x0100;
const IMAGE_LENGTH: u16 = 0x0101;
const BITS_PER_SAMPLE: u16 = 0x0102;
const COMPRESSION: u16 = 0x0103;
const PHOTOMETRIC: u16 = 0x0106;
const STRIP_OFFSETS: u16 = 0x0111;
const SAMPLES_PER_PIXEL: u16 = 0x0115;
const STRIP_BYTE_COUNTS: u16 = 0x0117;
const PLANAR_CONFIGURATION: u16 = 0x011c;
const TILE_OFFSETS: u16 = 0x0144;
const SUB_IFDS: u16 = 0x014a;
const JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
const SONY_CROP_SIZE: u16 = 0x74c8;
const EXIF_POINTER: u16 = 0x8769;
const GPS_POINTER: u16 = 0x8825;
const MAKER_NOTE: u16 = 0x927c;
const DEFAULT_CROP_SIZE: u16 = 0xc620;
const DNG_PRIVATE_DATA: u16 = 0xc634;
// A RW2's IFD0.
const RW2_SENSOR_WIDTH: u16 = 0x0002;
const RW2_SENSOR_HEIGHT: u16 = 0x0003;
const RW2_TOP_BORDER: u16 = 0x0004;
const RW2_LEFT_BORDER: u16 = 0x0005;
const RW2_BOTTOM_BORDER: u16 = 0x0006;
const RW2_RIGHT_BORDER: u16 = 0x0007;
const RW2_ISO: u16 = 0x0017;
const RW2_JPEG_FROM_RAW: u16 = 0x002e;

/// The most IFDs one file's walk visits, across every TIFF structure in it.
const MAX_IFDS: usize = 24;
/// The most IFDs of one chain (IFD0, IFD1, …) and SubIFDs of one IFD followed.
const MAX_CHAIN: usize = 8;
/// The most JPEG segments walked before the frame header.
const MAX_JPEG_SEGMENTS: usize = 64;
/// The most bytes scanned for one JPEG marker, fill and stray bytes included.
const MAX_MARKER_SCAN: u64 = 1024;
/// The most boxes of one ISO-BMFF level walked.
const MAX_BOXES: usize = 64;
/// The most RAF directory records walked.
const MAX_RAF_RECORDS: usize = 64;

/// Read `source`'s header, whatever its container.
pub(super) fn read(source: &mut impl Source) -> FileHeader {
    let mut found = Found::default();
    let mut walk = Walk::default();
    let container = recognize(source);
    match container {
        Container::Jpeg => jpeg(source, &mut walk, &mut found),
        Container::Tiff => raw_tiff(source, Magic::Standard, &mut walk, &mut found),
        Container::Orf => raw_tiff(source, Magic::Orf, &mut walk, &mut found),
        Container::Rw2 => rw2(source, &mut walk, &mut found),
        Container::Raf => raf(source, &mut walk, &mut found),
        Container::Cr3 => cr3(source, &mut found),
        Container::Unknown => {}
    }
    found.finish(container)
}

fn recognize(source: &mut impl Source) -> Container {
    let mut head = [0; 16];
    let length = usize::try_from(source.len())
        .unwrap_or(usize::MAX)
        .min(head.len());
    if !source.read(0, &mut head[..length]) {
        return Container::Unknown;
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Container::Jpeg;
    }
    if head.starts_with(b"FUJIFILMCCD-RAW ") {
        return Container::Raf;
    }
    if &head[4..12] == b"ftypcrx " {
        return Container::Cr3;
    }
    match Magic::of([head[0], head[1], head[2], head[3]]) {
        Some(Magic::Standard) => Container::Tiff,
        Some(Magic::Orf) => Container::Orf,
        Some(Magic::Rw2) => Container::Rw2,
        None => Container::Unknown,
    }
}

/// The IFDs already read, by absolute offset: none is read twice, and a file visits at most
/// [`MAX_IFDS`].
#[derive(Default)]
struct Walk {
    visited: [u64; MAX_IFDS],
    count: usize,
}

impl Walk {
    /// The IFD at `offset` of `tiff`, when it was not visited before and the walk has room.
    fn ifd(&mut self, source: &mut impl Source, tiff: &Tiff, offset: u32) -> Option<Ifd> {
        let at = tiff.absolute(offset);
        if offset == 0 || self.count == MAX_IFDS || self.visited[..self.count].contains(&at) {
            return None;
        }
        self.visited[self.count] = at;
        self.count += 1;
        tiff.ifd(source, offset)
    }
}

/// The largest full-resolution image IFD seen, and the crop it declares.
#[derive(Default)]
struct RawSize {
    size: Option<Dimensions>,
    crop: Option<Dimensions>,
}

impl RawSize {
    fn offer(&mut self, size: Dimensions, crop: Option<Dimensions>) {
        let area = |size: Dimensions| u64::from(size.width) * u64::from(size.height);
        if self.size.is_none_or(|kept| area(size) > area(kept)) {
            self.size = Some(size);
            self.crop = crop.filter(|crop| crop.width <= size.width && crop.height <= size.height);
        }
    }

    fn finish(self) -> Option<Dimensions> {
        self.crop.or(self.size)
    }
}

fn dimensions(width: Option<u32>, height: Option<u32>) -> Option<Dimensions> {
    let (width, height) = (width?, height?);
    (width > 0 && height > 0).then_some(Dimensions { width, height })
}

/// A standard, ORF or other TIFF-structured RAW: IFD0's fields, the Exif and GPS IFDs and the
/// maker note, then every image IFD for the dimensions and thumbnail.
fn raw_tiff(source: &mut impl Source, magic: Magic, walk: &mut Walk, found: &mut Found) {
    let end = source.len();
    let Some((tiff, ifd0)) = Tiff::open(source, 0, end, magic) else {
        return;
    };
    let Some(ifd0) = walk.ifd(source, &tiff, ifd0) else {
        return;
    };
    found.collect(source, &tiff, &ifd0, IfdKind::Primary);
    exif_and_gps(source, &tiff, &ifd0, walk, found);
    if let Some(private) = ifd0.find(DNG_PRIVATE_DATA) {
        makernote::dng_private(source, &tiff, &private, found);
    }
    let mut raw = RawSize::default();
    image(source, &tiff, &ifd0, Some(&mut raw), found);
    if let Some(entry) = ifd0.find(SUB_IFDS) {
        for index in 0..entry.count.min(MAX_CHAIN as u32) {
            let Some(offset) = tiff.uint_at(source, &entry, index) else {
                break;
            };
            if let Some(sub) = walk.ifd(source, &tiff, offset) {
                image(source, &tiff, &sub, Some(&mut raw), found);
            }
        }
    }
    let mut next = ifd0.next;
    for _ in 1..MAX_CHAIN {
        let Some(ifd) = walk.ifd(source, &tiff, next) else {
            break;
        };
        image(source, &tiff, &ifd, Some(&mut raw), found);
        next = ifd.next;
    }
    found.dimensions = raw.finish().or_else(|| found.exif_size());
}

/// The Exif IFD, its maker note and the GPS IFD that `ifd0` points at.
fn exif_and_gps(
    source: &mut impl Source,
    tiff: &Tiff,
    ifd0: &Ifd,
    walk: &mut Walk,
    found: &mut Found,
) {
    let pointer = |source: &mut _, tag| ifd0.find(tag).and_then(|entry| tiff.uint(source, &entry));
    if let Some(exif) = pointer(source, EXIF_POINTER).and_then(|at| walk.ifd(source, tiff, at)) {
        found.collect(source, tiff, &exif, IfdKind::Exif);
        if let Some(note) = exif.find(MAKER_NOTE) {
            makernote::read(source, tiff, &note, found);
        }
    }
    if let Some(gps) = pointer(source, GPS_POINTER).and_then(|at| walk.ifd(source, tiff, at)) {
        found.collect(source, tiff, &gps, IfdKind::Gps);
    }
}

/// One image IFD: offered as the full-resolution size when it is one (`raw`), and as a thumbnail
/// when it holds a JPEG (by JPEGInterchangeFormat, or one JPEG-compressed 8-bit strip) or one
/// uncompressed 8-bit RGB strip.
fn image(
    source: &mut impl Source,
    tiff: &Tiff,
    ifd: &Ifd,
    raw: Option<&mut RawSize>,
    found: &mut Found,
) {
    let mut uint = |tag| ifd.find(tag).and_then(|entry| tiff.uint(source, &entry));
    let size = dimensions(uint(IMAGE_WIDTH), uint(IMAGE_LENGTH));
    let full = uint(NEW_SUBFILE_TYPE).unwrap_or(0) == 0;
    let compression = uint(COMPRESSION).unwrap_or(1);
    let photometric = uint(PHOTOMETRIC);
    let samples = uint(SAMPLES_PER_PIXEL).unwrap_or(1);
    let planar = uint(PLANAR_CONFIGURATION).unwrap_or(1);
    let tiled = ifd.find(TILE_OFFSETS).is_some();
    let eight_bit = eight_bit(source, tiff, ifd);
    // A rendered image (RGB or YCbCr, a JPEG by pointer, or 8-bit samples that are not CFA or
    // linear raw data) is a preview, whatever its NewSubfileType says: a CR2's IFD0 declares none.
    let rendered = matches!(photometric, Some(2 | 6))
        || ifd.find(JPEG_INTERCHANGE_FORMAT).is_some()
        || (eight_bit && !matches!(photometric, Some(32803 | 34892)));
    if let (Some(raw), Some(size), true) = (raw, size, full && !rendered) {
        raw.offer(size, crop(source, tiff, ifd));
    }
    found.offer_jpeg_pointer(source, tiff, ifd);
    let Some(size) = size else {
        return;
    };
    if tiled || samples != 3 || planar != 1 || !eight_bit {
        return;
    }
    let Some((offset, length)) = strip(source, tiff, ifd) else {
        return;
    };
    match (compression, photometric) {
        (1, Some(2)) => found.offer_rgb(source, offset, length, size),
        (6 | 7, Some(2 | 6)) => found.offer_jpeg(source, offset, length, Some(size)),
        _ => {}
    }
}

/// Whether every BitsPerSample value is 8.
fn eight_bit(source: &mut impl Source, tiff: &Tiff, ifd: &Ifd) -> bool {
    let Some(entry) = ifd.find(BITS_PER_SAMPLE) else {
        return false;
    };
    (1..=4).contains(&entry.count)
        && (0..entry.count).all(|index| tiff.uint_at(source, &entry, index) == Some(8))
}

/// An IFD's one strip, as an absolute offset and length inside the structure.
fn strip(source: &mut impl Source, tiff: &Tiff, ifd: &Ifd) -> Option<(u64, u64)> {
    let offsets = ifd.find(STRIP_OFFSETS).filter(|entry| entry.count == 1)?;
    let counts = ifd
        .find(STRIP_BYTE_COUNTS)
        .filter(|entry| entry.count == 1)?;
    let offset = tiff.absolute(tiff.uint(source, &offsets)?);
    let length = u64::from(tiff.uint(source, &counts)?);
    (offset + length <= tiff.end()).then_some((offset, length))
}

/// The crop an image IFD declares: DNG's DefaultCropSize (two SHORTs, LONGs or RATIONALs, rounded)
/// or Sony's crop size (two LONGs).
fn crop(source: &mut impl Source, tiff: &Tiff, ifd: &Ifd) -> Option<Dimensions> {
    let entry = ifd
        .find(DEFAULT_CROP_SIZE)
        .or_else(|| ifd.find(SONY_CROP_SIZE))
        .filter(|entry| entry.count == 2)?;
    let mut side = |index| {
        let value = tiff.rational_at(source, &entry, index)?;
        let rounded = (u64::from(value.num) + u64::from(value.den) / 2) / u64::from(value.den);
        u32::try_from(rounded).ok()
    };
    dimensions(side(0), side(1))
}

/// An EXIF TIFF inside a JPEG (a JPEG file's, a RAF's or a RW2's embedded one): IFD0's fields, the
/// Exif and GPS IFDs and the maker note, and IFD1's thumbnail.
fn exif_tiff(source: &mut impl Source, tiff: &Tiff, ifd0: u32, walk: &mut Walk, found: &mut Found) {
    let Some(ifd0) = walk.ifd(source, tiff, ifd0) else {
        return;
    };
    found.collect(source, tiff, &ifd0, IfdKind::Primary);
    exif_and_gps(source, tiff, &ifd0, walk, found);
    if let Some(ifd1) = walk.ifd(source, tiff, ifd0.next) {
        image(source, tiff, &ifd1, None, found);
    }
}

/// What a JPEG walk found: the first `Exif` APP1's TIFF and IFD0 offset, and the frame size.
#[derive(Default)]
struct JpegWalk {
    exif: Option<(Tiff, u32)>,
    frame: Option<Dimensions>,
}

/// Walk the JPEG at `start..end` from SOI to its first scan, finding markers as libjpeg does (fill
/// bytes and stray bytes between segments skipped), for the first `Exif` APP1 and, when `frame`
/// is wanted, the frame header. It stops at a segment that does not fit.
fn jpeg_walk(source: &mut impl Source, start: u64, end: u64, frame: bool) -> JpegWalk {
    let mut walk = JpegWalk::default();
    if start + 2 > end || source.array(start) != Some([0xff, 0xd8]) {
        return walk;
    }
    let mut exif_seen = false;
    let mut at = start + 2;
    for _ in 0..MAX_JPEG_SEGMENTS {
        let Some((marker, after)) = next_marker(source, at, end) else {
            break;
        };
        at = after;
        match marker {
            // SOS or EOI: the header is over.
            0xda | 0xd9 => break,
            // Markers without a length.
            0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }
        let Some(length) = source.array(at).map(u16::from_be_bytes) else {
            break;
        };
        let segment_end = at + u64::from(length);
        if length < 2 || segment_end > end {
            break;
        }
        let payload = at + 2;
        if marker == 0xe1 && !exif_seen && source.array(payload) == Some(*b"Exif\0\0") {
            exif_seen = true;
            walk.exif = Tiff::open(source, payload + 6, segment_end, Magic::Standard);
        } else if frame && walk.frame.is_none() && is_frame(marker) {
            walk.frame = source.array(payload).and_then(|[_, h0, h1, w0, w1]| {
                dimensions(
                    Some(u32::from(u16::from_be_bytes([w0, w1]))),
                    Some(u32::from(u16::from_be_bytes([h0, h1]))),
                )
            });
        }
        at = segment_end;
        if exif_seen && (!frame || walk.frame.is_some()) {
            break;
        }
    }
    walk
}

/// SOF0 to SOF15, except DHT, JPG and DAC.
fn is_frame(marker: u8) -> bool {
    (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker)
}

/// The next marker code at or after `at` and where its segment's length starts.
fn next_marker(source: &mut impl Source, mut at: u64, end: u64) -> Option<(u8, u64)> {
    let stop = end.min(at.saturating_add(MAX_MARKER_SCAN));
    let mut byte = |at: &mut u64| {
        if *at >= stop {
            return None;
        }
        let [byte] = source.array(*at)?;
        *at += 1;
        Some(byte)
    };
    loop {
        let mut code = byte(&mut at)?;
        while code != 0xff {
            code = byte(&mut at)?;
        }
        while code == 0xff {
            code = byte(&mut at)?;
        }
        // A stuffed `FF 00` is not a marker.
        if code != 0 {
            return Some((code, at));
        }
    }
}

/// A JPEG file: its EXIF and its frame header.
fn jpeg(source: &mut impl Source, walk: &mut Walk, found: &mut Found) {
    let end = source.len();
    let jpeg = jpeg_walk(source, 0, end, true);
    if let Some((tiff, ifd0)) = jpeg.exif {
        exif_tiff(source, &tiff, ifd0, walk, found);
    }
    found.dimensions = jpeg.frame;
}

/// A RW2: IFD0 with Panasonic's own ISO and sensor borders, the Exif and GPS IFDs, then the
/// embedded JPEG's EXIF (where Panasonic keeps the fields its own Exif IFD lacks, such as ISO and
/// the maker note) and its IFD1 thumbnail.
fn rw2(source: &mut impl Source, walk: &mut Walk, found: &mut Found) {
    let end = source.len();
    let Some((tiff, ifd0)) = Tiff::open(source, 0, end, Magic::Rw2) else {
        return;
    };
    let Some(ifd0) = walk.ifd(source, &tiff, ifd0) else {
        return;
    };
    found.collect(source, &tiff, &ifd0, IfdKind::Primary);
    let mut uint = |tag| ifd0.find(tag).and_then(|entry| tiff.uint(source, &entry));
    found.iso.panasonic = uint(RW2_ISO);
    let bordered = match [
        uint(RW2_TOP_BORDER),
        uint(RW2_LEFT_BORDER),
        uint(RW2_BOTTOM_BORDER),
        uint(RW2_RIGHT_BORDER),
    ] {
        [Some(top), Some(left), Some(bottom), Some(right)] => {
            dimensions(right.checked_sub(left), bottom.checked_sub(top))
        }
        _ => None,
    };
    found.dimensions =
        bordered.or_else(|| dimensions(uint(RW2_SENSOR_WIDTH), uint(RW2_SENSOR_HEIGHT)));
    exif_and_gps(source, &tiff, &ifd0, walk, found);
    let embedded = ifd0
        .find(RW2_JPEG_FROM_RAW)
        .and_then(|entry| tiff.blob(&entry));
    if let Some((start, length)) = embedded {
        let at = tiff.absolute(start);
        let jpeg = jpeg_walk(source, at, at + u64::from(length), false);
        if let Some((exif, ifd0)) = jpeg.exif {
            exif_tiff(source, &exif, ifd0, walk, found);
        }
    }
}

/// A RAF: the EXIF of the JPEG it embeds (by the header's big-endian offset and length, as the
/// export reads it), then the RAF directory's image size.
fn raf(source: &mut impl Source, walk: &mut Walk, found: &mut Found) {
    let Some(words) = source.array::<16>(84) else {
        return;
    };
    let word = |at: usize| {
        u64::from(u32::from_be_bytes([
            words[at],
            words[at + 1],
            words[at + 2],
            words[at + 3],
        ]))
    };
    let (jpeg_at, jpeg_length) = (word(0), word(4));
    let (directory, directory_length) = (word(8), word(12));
    let end = source.len();
    if jpeg_length > 0 && jpeg_at + jpeg_length <= end {
        let jpeg = jpeg_walk(source, jpeg_at, jpeg_at + jpeg_length, false);
        if let Some((tiff, ifd0)) = jpeg.exif {
            exif_tiff(source, &tiff, ifd0, walk, found);
        }
    }
    if directory_length > 0 && directory + directory_length <= end {
        found.dimensions = raf_size(source, directory, directory + directory_length);
    }
}

/// The RAF directory's cropped image size (0x0111), else its full size (0x0100): big-endian
/// records of a tag, a length and the data, sorted by tag, each size stored height first.
fn raf_size(source: &mut impl Source, directory: u64, end: u64) -> Option<Dimensions> {
    let count = source.array(directory).map(u32::from_be_bytes)?;
    let mut at = directory + 4;
    let (mut full, mut cropped) = (None, None);
    for _ in 0..count.min(MAX_RAF_RECORDS as u32) {
        let [t0, t1, s0, s1] = source.array(at)?;
        let (tag, length) = (
            u16::from_be_bytes([t0, t1]),
            u64::from(u16::from_be_bytes([s0, s1])),
        );
        let data = at + 4;
        if data + length > end || tag > 0x0111 {
            break;
        }
        if length == 4 && [0x0100, 0x0111].contains(&tag) {
            let [h0, h1, w0, w1] = source.array(data)?;
            let size = dimensions(
                Some(u32::from(u16::from_be_bytes([w0, w1]))),
                Some(u32::from(u16::from_be_bytes([h0, h1]))),
            );
            if tag == 0x0100 {
                full = size;
            } else {
                cropped = size;
            }
        }
        at = data + length;
    }
    cropped.or(full)
}

/// One ISO-BMFF box: its type and where its body and the box end.
struct Mp4Box {
    kind: [u8; 4],
    body: u64,
    end: u64,
}

/// The box at `at` inside a parent ending at `end`: a 32-bit size, or 1 and a 64-bit size after
/// the type, or 0 for "to the parent's end"; a `uuid` box's body starts after its 16-byte type.
fn mp4_box(source: &mut impl Source, at: u64, end: u64) -> Option<Mp4Box> {
    let header: [u8; 8] = source.array(at)?;
    let kind = [header[4], header[5], header[6], header[7]];
    let (size, mut body) = match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
        0 => (end.checked_sub(at)?, at + 8),
        1 => (source.array(at + 8).map(u64::from_be_bytes)?, at + 16),
        size => (u64::from(size), at + 8),
    };
    if kind == *b"uuid" {
        body += 16;
    }
    let box_end = at.checked_add(size)?;
    (body <= box_end && box_end <= end).then_some(Mp4Box {
        kind,
        body,
        end: box_end,
    })
}

/// The first box of `kind` inside `start..end` (for `uuid`, with `user_type`), within
/// [`MAX_BOXES`].
fn find_box(
    source: &mut impl Source,
    start: u64,
    end: u64,
    kind: &[u8; 4],
    user_type: Option<&[u8; 16]>,
) -> Option<Mp4Box> {
    let mut at = start;
    for _ in 0..MAX_BOXES {
        let found = mp4_box(source, at, end)?;
        // A `uuid` box's user type is the 16 bytes before its body.
        if found.kind == *kind
            && user_type.is_none_or(|user_type| source.array(found.body - 16) == Some(*user_type))
        {
            return Some(found);
        }
        at = found.end;
    }
    None
}

/// The Canon metadata box inside `moov`.
const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];

/// A CR3: `moov`'s Canon box holds `CMT1` (IFD0 as a TIFF), `CMT2` (the Exif IFD), `CMT3` (the
/// maker note), `CMT4` (the GPS IFD) and `THMB` (the thumbnail JPEG with its size).
fn cr3(source: &mut impl Source, found: &mut Found) {
    let end = source.len();
    let Some(moov) = find_box(source, 0, end, b"moov", None) else {
        return;
    };
    let Some(canon) = find_box(source, moov.body, moov.end, b"uuid", Some(&CANON_UUID)) else {
        return;
    };
    let mut at = canon.body;
    for _ in 0..MAX_BOXES {
        let Some(child) = mp4_box(source, at, canon.end) else {
            break;
        };
        at = child.end;
        if child.kind == *b"THMB" {
            thumbnail_box(source, &child, found);
            continue;
        }
        let kind = match &child.kind {
            b"CMT1" => IfdKind::Primary,
            b"CMT2" => IfdKind::Exif,
            b"CMT4" => IfdKind::Gps,
            b"CMT3" => {
                if let Some((tiff, ifd0)) =
                    Tiff::open(source, child.body, child.end, Magic::Standard)
                {
                    makernote::canon(source, &tiff, ifd0, found);
                }
                continue;
            }
            _ => continue,
        };
        let Some((tiff, ifd0)) = Tiff::open(source, child.body, child.end, Magic::Standard) else {
            continue;
        };
        let Some(ifd) = tiff.ifd(source, ifd0) else {
            continue;
        };
        found.collect(source, &tiff, &ifd, kind);
        if kind == IfdKind::Primary && found.dimensions.is_none() {
            let mut uint = |tag| ifd.find(tag).and_then(|entry| tiff.uint(source, &entry));
            found.dimensions = dimensions(uint(IMAGE_WIDTH), uint(IMAGE_LENGTH));
        }
    }
    if found.dimensions.is_none() {
        found.dimensions = found.exif_size();
    }
}

/// A CR3's `THMB`. Version 0: a version and flags, the width and height, the JPEG's length and
/// four reserved bytes, then the JPEG. Version 1 (the EOS R8 and R5 Mark II samples) holds an
/// HEVC-coded thumbnail instead, which is not offered.
fn thumbnail_box(source: &mut impl Source, thumbnail: &Mp4Box, found: &mut Found) {
    let Some(header) = source.array::<16>(thumbnail.body) else {
        return;
    };
    if header[0] != 0 {
        return;
    }
    let size = dimensions(
        Some(u32::from(u16::from_be_bytes([header[4], header[5]]))),
        Some(u32::from(u16::from_be_bytes([header[6], header[7]]))),
    );
    let length = u64::from(u32::from_be_bytes([
        header[8], header[9], header[10], header[11],
    ]));
    let jpeg = thumbnail.body + 16;
    if jpeg + length <= thumbnail.end {
        found.offer_jpeg(source, jpeg, length, size);
    }
}
