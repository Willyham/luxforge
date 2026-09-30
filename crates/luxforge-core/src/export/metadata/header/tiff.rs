//! A tolerant TIFF walker over a [`Source`]: one TIFF structure at a base offset, its IFD tables
//! read whole and bounded, and the value types cameras actually write. Unlike the export's strict
//! reader it accepts SHORT for LONG, SRATIONAL for RATIONAL where the sign is plain, and text in
//! BYTE, UNDEFINED or UTF-8 as well as ASCII.

use super::source::Source;
use super::{Rational, SignedRational};

/// The most entries one IFD may declare; a larger table is not read at all.
pub(super) const MAX_IFD_ENTRIES: usize = 256;

/// The most bytes of one text value read; a longer value is cut at its first NUL within them, or
/// dropped when it has none.
pub(super) const MAX_TEXT_BYTES: usize = 256;

// TIFF field types.
const BYTE: u16 = 1;
const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const RATIONAL: u16 = 5;
const UNDEFINED: u16 = 7;
const SRATIONAL: u16 = 10;
const IFD: u16 = 13;
const UTF8: u16 = 129;

/// The second header word each container family accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Magic {
    /// `II*\0` or `MM\0*`: TIFF, and the RAW formats built on it (NEF, ARW, CR2, DNG, PEF).
    Standard,
    /// `IIRO`, `IIRS` or `MMOR`: Olympus ORF.
    Orf,
    /// `IIU\0`: Panasonic RW2.
    Rw2,
}

impl Magic {
    /// The family of a file's first four bytes, when it is one of the TIFF-structured ones.
    pub(super) fn of(head: [u8; 4]) -> Option<Self> {
        match &head {
            b"II*\0" | b"MM\0*" => Some(Self::Standard),
            b"IIRO" | b"IIRS" | b"MMOR" => Some(Self::Orf),
            b"IIU\0" => Some(Self::Rw2),
            _ => None,
        }
    }
}

/// One TIFF structure: its offsets are relative to `base`, and nothing it names may lie at or past
/// `end` (the end of the file, or of the segment or box that holds it).
#[derive(Clone, Copy, Debug)]
pub(super) struct Tiff {
    base: u64,
    end: u64,
    big_endian: bool,
}

/// One 12-byte IFD entry: its tag, type and count, its value field (the value itself when it fits
/// four bytes, else the value's offset) and where that field is, relative to the TIFF's base.
#[derive(Clone, Copy, Debug)]
pub(super) struct Entry {
    pub(super) tag: u16,
    pub(super) kind: u16,
    pub(super) count: u32,
    field: [u8; 4],
    field_at: u32,
}

/// One IFD's table, copied whole in one read.
pub(super) struct Ifd {
    table: [u8; MAX_IFD_ENTRIES * 12],
    count: usize,
    /// Where the table's entries start, relative to the TIFF's base.
    at: u32,
    big_endian: bool,
    /// The next IFD's offset, 0 when none or unreadable.
    pub(super) next: u32,
}

impl Ifd {
    pub(super) fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        (0..self.count).map(move |i| {
            let raw = &self.table[i * 12..i * 12 + 12];
            let u16_at = |at: usize| order_u16([raw[at], raw[at + 1]], self.big_endian);
            Entry {
                tag: u16_at(0),
                kind: u16_at(2),
                count: order_u32([raw[4], raw[5], raw[6], raw[7]], self.big_endian),
                field: [raw[8], raw[9], raw[10], raw[11]],
                // At most 256 entries past a u32 offset the table was read at.
                field_at: self.at.wrapping_add((i * 12 + 8) as u32),
            }
        })
    }

    /// The first entry tagged `tag`.
    pub(super) fn find(&self, tag: u16) -> Option<Entry> {
        self.entries().find(|entry| entry.tag == tag)
    }
}

fn order_u16(bytes: [u8; 2], big_endian: bool) -> u16 {
    if big_endian {
        u16::from_be_bytes(bytes)
    } else {
        u16::from_le_bytes(bytes)
    }
}

fn order_u32(bytes: [u8; 4], big_endian: bool) -> u32 {
    if big_endian {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    }
}

/// The bytes one value of `kind` takes, for the types a header reads.
fn unit(kind: u16) -> Option<u32> {
    match kind {
        BYTE | ASCII | UNDEFINED | UTF8 | 6 => Some(1),
        SHORT | 8 => Some(2),
        LONG | IFD | 9 => Some(4),
        RATIONAL | SRATIONAL => Some(8),
        _ => None,
    }
}

impl Tiff {
    /// The TIFF whose header is at `base`, when its first four bytes are `magic`'s: the structure
    /// and IFD0's offset.
    pub(super) fn open(
        source: &mut impl Source,
        base: u64,
        end: u64,
        magic: Magic,
    ) -> Option<(Self, u32)> {
        let header: [u8; 8] = source.array(base)?;
        let head = [header[0], header[1], header[2], header[3]];
        if Magic::of(head) != Some(magic) {
            return None;
        }
        let tiff = Self::with_order(base, end, header[0] == b'M');
        let ifd0 = order_u32(
            [header[4], header[5], header[6], header[7]],
            tiff.big_endian,
        );
        Some((tiff, ifd0))
    }

    /// A structure of known byte order without a header of its own, as some maker notes are.
    pub(super) fn with_order(base: u64, end: u64, big_endian: bool) -> Self {
        Self {
            base,
            end,
            big_endian,
        }
    }

    pub(super) fn end(&self) -> u64 {
        self.end
    }

    /// The absolute file offset of `offset`.
    pub(super) fn absolute(&self, offset: u32) -> u64 {
        self.base + u64::from(offset)
    }

    /// Fill `out` from `offset`, when the range lies before the structure's end.
    pub(super) fn read(&self, source: &mut impl Source, offset: u32, out: &mut [u8]) -> bool {
        let at = self.absolute(offset);
        at + out.len() as u64 <= self.end && source.read(at, out)
    }

    fn u16(&self, source: &mut impl Source, offset: u32) -> Option<u16> {
        let mut bytes = [0; 2];
        self.read(source, offset, &mut bytes)
            .then(|| order_u16(bytes, self.big_endian))
    }

    fn u32(&self, source: &mut impl Source, offset: u32) -> Option<u32> {
        let mut bytes = [0; 4];
        self.read(source, offset, &mut bytes)
            .then(|| order_u32(bytes, self.big_endian))
    }

    /// The IFD at `offset`, when its whole table lies inside the structure and declares at most
    /// [`MAX_IFD_ENTRIES`] entries.
    pub(super) fn ifd(&self, source: &mut impl Source, offset: u32) -> Option<Ifd> {
        if offset == 0 {
            return None;
        }
        let count = usize::from(self.u16(source, offset)?);
        if count > MAX_IFD_ENTRIES {
            return None;
        }
        let at = offset.checked_add(2)?;
        let mut ifd = Ifd {
            table: [0; MAX_IFD_ENTRIES * 12],
            count,
            at,
            big_endian: self.big_endian,
            next: 0,
        };
        if !self.read(source, at, &mut ifd.table[..count * 12]) {
            return None;
        }
        // At most 3072 bytes past a u32 offset.
        let link = u32::try_from(u64::from(at) + count as u64 * 12).ok()?;
        ifd.next = self.u32(source, link).unwrap_or(0);
        Some(ifd)
    }

    /// Where `entry`'s value starts, relative to the base: inline when its `count` values fit four
    /// bytes, else at the offset the entry names; only when the whole value lies inside the
    /// structure. `limit` caps how many values are wanted.
    fn value_at(&self, entry: &Entry, limit: u32) -> Option<(u32, u32)> {
        let unit = unit(entry.kind)?;
        let whole = u64::from(entry.count) * u64::from(unit);
        let start = if whole <= 4 {
            entry.field_at
        } else {
            order_u32(entry.field, self.big_endian)
        };
        (self.absolute(start) + whole <= self.end).then_some((start, entry.count.min(limit)))
    }

    /// Where `entry`'s value starts and how many bytes it takes, for a blob such as an embedded
    /// JPEG or a maker note: BYTE or UNDEFINED data.
    pub(super) fn blob(&self, entry: &Entry) -> Option<(u32, u32)> {
        if ![BYTE, UNDEFINED].contains(&entry.kind) || entry.count == 0 {
            return None;
        }
        self.value_at(entry, u32::MAX)
    }

    /// Unsigned integer `index` of a BYTE, SHORT, LONG or IFD value.
    pub(super) fn uint_at(
        &self,
        source: &mut impl Source,
        entry: &Entry,
        index: u32,
    ) -> Option<u32> {
        if index >= entry.count {
            return None;
        }
        let (start, _) = self.value_at(entry, u32::MAX)?;
        match entry.kind {
            BYTE => {
                let mut byte = [0];
                self.read(source, start.checked_add(index)?, &mut byte)
                    .then_some(u32::from(byte[0]))
            }
            SHORT => self
                .u16(source, start.checked_add(index.checked_mul(2)?)?)
                .map(u32::from),
            LONG | IFD => self.u32(source, start.checked_add(index.checked_mul(4)?)?),
            _ => None,
        }
    }

    /// The first unsigned integer of a BYTE, SHORT, LONG or IFD value.
    pub(super) fn uint(&self, source: &mut impl Source, entry: &Entry) -> Option<u32> {
        self.uint_at(source, entry, 0)
    }

    /// Value `index` of a RATIONAL, an SRATIONAL that is not negative, or a SHORT or LONG as a
    /// whole number; never with a zero denominator.
    pub(super) fn rational_at(
        &self,
        source: &mut impl Source,
        entry: &Entry,
        index: u32,
    ) -> Option<Rational> {
        match entry.kind {
            RATIONAL | SRATIONAL => {
                if index >= entry.count {
                    return None;
                }
                let (start, _) = self.value_at(entry, u32::MAX)?;
                let at = start.checked_add(index.checked_mul(8)?)?;
                let [num, den] = [self.u32(source, at)?, self.u32(source, at.checked_add(4)?)?];
                let negative =
                    entry.kind == SRATIONAL && (num.cast_signed() < 0 || den.cast_signed() < 0);
                (!negative && den != 0).then_some(Rational { num, den })
            }
            BYTE | SHORT | LONG => Some(Rational {
                num: self.uint_at(source, entry, index)?,
                den: 1,
            }),
            _ => None,
        }
    }

    pub(super) fn rational(&self, source: &mut impl Source, entry: &Entry) -> Option<Rational> {
        self.rational_at(source, entry, 0)
    }

    /// The first value of an SRATIONAL, or of a RATIONAL read as the same bits signed; never with
    /// a zero denominator.
    pub(super) fn signed_rational(
        &self,
        source: &mut impl Source,
        entry: &Entry,
    ) -> Option<SignedRational> {
        if ![RATIONAL, SRATIONAL].contains(&entry.kind) || entry.count == 0 {
            return None;
        }
        let (start, _) = self.value_at(entry, u32::MAX)?;
        let [num, den] = [
            self.u32(source, start)?,
            self.u32(source, start.checked_add(4)?)?,
        ];
        (den != 0).then_some(SignedRational {
            num: num.cast_signed(),
            den: den.cast_signed(),
        })
    }

    /// The text of an ASCII, UTF-8, BYTE or UNDEFINED value: up to its first NUL, trimmed of
    /// surrounding whitespace, lossily decoded; none when empty, or when longer than
    /// [`MAX_TEXT_BYTES`] without a NUL.
    pub(super) fn text(&self, source: &mut impl Source, entry: &Entry) -> Option<String> {
        let mut buffer = [0; MAX_TEXT_BYTES];
        let raw = self.raw_text(source, entry, &mut buffer)?;
        let text = String::from_utf8_lossy(raw);
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// The bytes of a text value up to its first NUL, trimmed of ASCII whitespace, in `buffer`.
    pub(super) fn raw_text<'b>(
        &self,
        source: &mut impl Source,
        entry: &Entry,
        buffer: &'b mut [u8; MAX_TEXT_BYTES],
    ) -> Option<&'b [u8]> {
        if ![ASCII, UTF8, BYTE, UNDEFINED].contains(&entry.kind) {
            return None;
        }
        let (start, count) = self.value_at(entry, MAX_TEXT_BYTES as u32)?;
        let raw = &mut buffer[..count as usize];
        if !self.read(source, start, raw) {
            return None;
        }
        let raw = &buffer[..count as usize];
        let text = match raw.iter().position(|byte| *byte == 0) {
            Some(nul) => &raw[..nul],
            None if entry.count as usize > MAX_TEXT_BYTES => return None,
            None => raw,
        };
        Some(text.trim_ascii())
    }
}
