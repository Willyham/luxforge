//! The bounded EXIF reader: a JPEG's APP1 segment, a RAF's embedded JPEG and a TIFF's IFD0, Exif
//! and GPS IFDs. Every offset and count is checked against the buffer before it is used, nothing is
//! allocated from a declared count, and no IFD is read twice.

use super::{
    ASCII, BYTE, COPYRIGHT, EXIF_POINTER, FIELDS, GPS_POINTER, IFD, Ifd, Kind, LONG, MAX_ASCII,
    RATIONAL, SHORT, SRATIONAL, Value,
};

/// The IFD0 tag of the EXIF orientation, read only by [`orientation`].
const ORIENTATION: u16 = 0x0112;

/// The most entries one IFD may declare; a larger table is not read at all.
const MAX_ENTRIES: usize = 256;

/// A RAF's header, then the big-endian offset and length of the JPEG it embeds.
const RAF_MAGIC: &[u8] = b"FUJIFILMCCD-RAW";
const RAF_JPEG_OFFSET: usize = 84;
const RAF_JPEG_LENGTH: usize = 88;

pub(super) fn is_tiff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")
}

/// The TIFF payload of a JPEG's first `Exif` APP1 segment before its first scan, as the codec's
/// container walk finds the segments; none where that walk stops early. A segment's length is 16
/// bits, so the payload is under 64 KiB.
pub(super) fn jpeg_exif(bytes: &[u8]) -> Option<&[u8]> {
    luxforge_jpeg::segments(bytes)
        .map_while(Result::ok)
        .filter(|segment| segment.marker == 0xe1)
        .find_map(|segment| segment.payload.strip_prefix(b"Exif\0\0"))
}

/// IFD0's Orientation in one TIFF structure: the first entry tagged 0x0112 that is one SHORT, when
/// its value is 1 to 8.
pub(super) fn orientation(bytes: &[u8]) -> Option<u8> {
    let (tiff, ifd0) = Tiff::new(bytes)?;
    let entry = tiff
        .entries(ifd0)?
        .find(|entry| entry.tag == ORIENTATION && entry.kind == SHORT && entry.count == 1)?;
    let value = tiff.u16(entry.at + 8)?;
    u8::try_from(value)
        .ok()
        .filter(|value| (1..=8).contains(value))
}

/// The JPEG a RAF embeds, when its header's offset and length lie inside the file.
pub(super) fn raf_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(RAF_MAGIC) {
        return None;
    }
    let word = |at: usize| {
        let word = bytes.get(at..at + 4)?;
        usize::try_from(u32::from_be_bytes(word.try_into().ok()?)).ok()
    };
    let offset = word(RAF_JPEG_OFFSET)?;
    let length = word(RAF_JPEG_LENGTH)?;
    bytes.get(offset..offset.checked_add(length)?)
}

/// The kept fields of one TIFF structure, in the order found: IFD0, then the Exif and GPS IFDs it
/// points at. A table out of bounds or too large contributes nothing; a field of the wrong type or
/// count, or whose value lies outside the buffer, is skipped.
pub(super) fn fields(bytes: &[u8]) -> Vec<(usize, Value)> {
    let mut fields = Vec::new();
    let Some((tiff, ifd0)) = Tiff::new(bytes) else {
        return fields;
    };
    let mut visited = Vec::with_capacity(3);
    let mut pointers = [None, None];
    tiff.read(ifd0, Ifd::Primary, &mut visited, &mut fields, &mut pointers);
    let [exif, gps] = pointers;
    if let Some(exif) = exif {
        tiff.read(exif, Ifd::Exif, &mut visited, &mut fields, &mut pointers);
    }
    if let Some(gps) = gps {
        tiff.read(gps, Ifd::Gps, &mut visited, &mut fields, &mut pointers);
    }
    fields
}

struct Tiff<'a> {
    bytes: &'a [u8],
    big_endian: bool,
}

/// One 12-byte IFD entry: its tag, type and count, and where the entry starts.
struct Entry {
    at: usize,
    tag: u16,
    kind: u16,
    count: u32,
}

impl<'a> Tiff<'a> {
    fn new(bytes: &'a [u8]) -> Option<(Self, u32)> {
        let big_endian = match bytes.get(..4)? {
            b"II*\0" => false,
            b"MM\0*" => true,
            _ => return None,
        };
        let tiff = Self { bytes, big_endian };
        let ifd0 = tiff.u32(4)?;
        Some((tiff, ifd0))
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let bytes = self.bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let bytes = self.bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }

    /// Read the IFD at `offset` once, keeping its table fields; IFD0 also yields the Exif and GPS
    /// pointers. Later IFDs (IFD1's thumbnail) and sub-IFDs are never followed.
    fn read(
        &self,
        offset: u32,
        ifd: Ifd,
        visited: &mut Vec<u32>,
        fields: &mut Vec<(usize, Value)>,
        pointers: &mut [Option<u32>; 2],
    ) {
        if visited.contains(&offset) {
            return;
        }
        visited.push(offset);
        let Some(entries) = self.entries(offset) else {
            return;
        };
        for entry in entries {
            if ifd == Ifd::Primary && [EXIF_POINTER, GPS_POINTER].contains(&entry.tag) {
                let slot = &mut pointers[usize::from(entry.tag == GPS_POINTER)];
                if slot.is_none() && [LONG, IFD].contains(&entry.kind) && entry.count == 1 {
                    *slot = self.u32(entry.at + 8);
                }
                continue;
            }
            let Some(index) = FIELDS
                .iter()
                .position(|field| field.ifd == ifd && field.tag == entry.tag)
            else {
                continue;
            };
            if let Some(value) = self.value(&entry, FIELDS[index].kind) {
                fields.push((index, value));
            }
        }
    }

    /// The entries of the IFD at `offset`, when its whole table lies inside the buffer and declares
    /// at most [`MAX_ENTRIES`].
    fn entries(&self, offset: u32) -> Option<impl Iterator<Item = Entry> + '_> {
        let offset = usize::try_from(offset).ok()?;
        let count = usize::from(self.u16(offset)?);
        if count > MAX_ENTRIES {
            return None;
        }
        let table = offset.checked_add(2)?;
        if table.checked_add(count * 12)? > self.bytes.len() {
            return None;
        }
        Some((0..count).filter_map(move |i| {
            let at = table + i * 12;
            Some(Entry {
                at,
                tag: self.u16(at)?,
                kind: self.u16(at + 2)?,
                count: self.u32(at + 4)?,
            })
        }))
    }

    /// Where `entry`'s value of `count` units of `unit` bytes starts: inline when it fits four
    /// bytes, else at the offset the entry names, when the whole value lies inside the buffer.
    fn start(&self, entry: &Entry, unit: usize) -> Option<usize> {
        let length = usize::try_from(entry.count).ok()?.checked_mul(unit)?;
        let start = if length <= 4 {
            entry.at + 8
        } else {
            usize::try_from(self.u32(entry.at + 8)?).ok()?
        };
        (start.checked_add(length)? <= self.bytes.len()).then_some(start)
    }

    /// `entry`'s value when it has exactly the type and count `kind` expects.
    fn value(&self, entry: &Entry, kind: Kind) -> Option<Value> {
        let count = entry.count;
        match kind {
            Kind::Ascii(fixed) => {
                if entry.kind != ASCII || count == 0 || count > MAX_ASCII {
                    return None;
                }
                let start = self.start(entry, 1)?;
                let raw = &self.bytes[start..start + count as usize];
                let text = ascii(raw, entry.tag == COPYRIGHT);
                if text.is_empty() || fixed.is_some_and(|length| text.len() != length) {
                    return None;
                }
                Some(Value::Ascii(text.into()))
            }
            Kind::Bytes(n) => {
                if entry.kind != BYTE || count != n {
                    return None;
                }
                let start = self.start(entry, 1)?;
                Some(Value::Bytes(self.bytes[start..start + n as usize].into()))
            }
            Kind::Short => {
                if entry.kind != SHORT || count != 1 {
                    return None;
                }
                Some(Value::Short(self.u16(self.start(entry, 2)?)?))
            }
            Kind::Rational(n) => {
                if entry.kind != RATIONAL || count != n {
                    return None;
                }
                let start = self.start(entry, 8)?;
                (0..n as usize)
                    .map(|i| {
                        let at = start + i * 8;
                        Some([self.u32(at)?, self.u32(at + 4)?])
                    })
                    .collect::<Option<Box<_>>>()
                    .map(Value::Rational)
            }
            Kind::SRational => {
                if entry.kind != SRATIONAL || count != 1 {
                    return None;
                }
                let start = self.start(entry, 8)?;
                // An SRATIONAL is two SLONGs: the same bytes as two LONGs, reinterpreted.
                let [num, den] = [self.u32(start)?, self.u32(start + 4)?];
                Some(Value::SRational([num.cast_signed(), den.cast_signed()]))
            }
        }
    }
}

/// The text of a NUL-terminated ASCII value: up to its first NUL, except Copyright, which keeps
/// its NUL-separated parts; trailing NULs and spaces are trimmed either way.
fn ascii(raw: &[u8], parts: bool) -> &[u8] {
    let text = if parts {
        raw
    } else {
        raw.split(|byte| *byte == 0).next().unwrap_or_default()
    };
    let end = text
        .iter()
        .rposition(|byte| *byte != 0 && *byte != b' ')
        .map_or(0, |last| last + 1);
    &text[..end]
}
