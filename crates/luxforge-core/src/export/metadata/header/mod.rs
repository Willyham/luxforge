//! Typed capture metadata read from a bounded prefix of a photo file: what indexing reads for every
//! file it lists (`docs/design/catalog.md#browsing-the-filesystem`), and never image data.
//!
//! Contract:
//! - [`read_header`] reads an open file's first [`HEADER_HEAD_BYTES`] in one read, then only
//!   ranges the container itself names — an IFD, a value, a box, the next JPEG segment, a RAF's
//!   embedded JPEG or directory — in [`CHUNK_BYTES`]-aligned chunks, each read once and kept, until
//!   the parsers are done or [`MAX_HEADER_BYTES`] or [`MAX_HEADER_READS`] is spent. Past the budget
//!   it returns what it has and says so ([`HeaderRead::capped`]). It never reads image data, never
//!   allocates from a declared count, never panics on malformed input, and returns an I/O error as
//!   an error.
//! - [`FileHeader::from_bytes`] runs the same parsers over a whole file in memory. A bounded read
//!   that was not capped equals it.
//! - Containers: JPEG (the first `Exif` APP1 and the frame header), TIFF and the RAW formats built
//!   on it (NEF, ARW, CR2, DNG, PEF), ORF and RW2 (TIFF with their own magic), RAF (the embedded
//!   JPEG's EXIF and the RAF's own directory) and CR3 (the ISO-BMFF `CMT1`–`CMT4` and `THMB`
//!   boxes). Anything else is [`Container::Unknown`] with no fields.
//! - Every field is `None` when absent or invalid; nothing is inferred (an offset-less time stays
//!   offset-less, never UTC).
//!
//! The export's reader beside this one ([`super::CaptureMetadata`]) is independent and strict; this
//! reader is tolerant of the type variants cameras write, and changes nothing the export keeps.

mod container;
mod fields;
mod makernote;
mod source;
mod tiff;

#[cfg(test)]
mod authentic;
#[cfg(test)]
mod tests;

use std::io::{self, Read, Seek};

/// The first read of every file. What organizing needs is usually within a file's first 64 KiB
/// (`docs/design/catalog.md#keeping-up-with-the-disk`): a JPEG's EXIF segment is at most 64 KiB, and
/// the corpus's CR2, CR3, ORF, RW2, RAF (EXIF) and DNG metadata, and most of NEF's and ARW's, lie
/// inside it.
pub const HEADER_HEAD_BYTES: usize = 64 * 1024;

/// The alignment and least size of every read past the head: one page, which also holds the
/// largest IFD table read (256 entries of 12 bytes).
pub const CHUNK_BYTES: usize = 4 * 1024;

/// The most bytes one header read takes, head included. The authentic corpus (117 RAW files of
/// every admitted container, and camera JPEGs) needs at most 84 KiB: the head plus up to four
/// chunks, for a NEF's GPS IFD, SubIFDs and maker-note preview IFD, an ARW's SubIFD, a PEF's IFD1
/// or a RAF's directory past its embedded JPEG. Three times that leaves room for containers laid
/// out less kindly while keeping two index workers' reads small.
pub const MAX_HEADER_BYTES: usize = 256 * 1024;

/// The most reads one header read makes, head included. The corpus needs at most five; past this
/// a pathological layout (IFDs scattered across the file) stops rather than seeking on.
pub const MAX_HEADER_READS: usize = 16;

/// The largest embedded JPEG counted as a thumbnail: the small preview beside the metadata, never
/// the camera's large preview, which is the preview lane's.
pub const MAX_THUMBNAIL_JPEG_BYTES: u64 = 512 * 1024;

/// The longest side of an uncompressed RGB thumbnail.
pub const MAX_THUMBNAIL_SIDE: u32 = 1024;

/// The container a header was read from, recognized by its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    Jpeg,
    /// TIFF with the standard magic: NEF, ARW, CR2, DNG, PEF.
    Tiff,
    Orf,
    Rw2,
    Raf,
    Cr3,
    Unknown,
}

/// A calendar date and time of day as a camera wrote it, validated: a real date, hours 0–23,
/// minutes and seconds 0–59.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Which EXIF time a [`CaptureTime`] came from, with its paired subsecond and offset tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeSource {
    /// DateTimeOriginal (0x9003), SubSecTimeOriginal (0x9291), OffsetTimeOriginal (0x9011).
    Original,
    /// DateTimeDigitized (0x9004), SubSecTimeDigitized (0x9292), OffsetTimeDigitized (0x9012).
    Digitized,
    /// IFD0's DateTime (0x0132), SubSecTime (0x9290), OffsetTime (0x9010).
    Modified,
}

/// When the photograph was taken, in the camera's local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CaptureTime {
    pub local: DateTime,
    /// The fraction of the second in nanoseconds (below 1,000,000,000), from the source's
    /// SubSecTime digits read as a decimal fraction: "42" is 0.42 s.
    pub subsec_nanos: Option<u32>,
    /// Minutes east of UTC from the source's OffsetTime ("+01:00" is 60, "-02:30" is -150). `None`
    /// is unknown, never UTC.
    pub offset_minutes: Option<i16>,
    pub source: TimeSource,
}

/// The GPS receiver's UTC date and time (GPSDateStamp and GPSTimeStamp).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpsTime {
    pub utc: DateTime,
    /// The fraction of the second in nanoseconds.
    pub nanos: u32,
}

/// An unsigned EXIF rational as stored, never with a zero denominator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub fn value(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }
}

/// A signed EXIF rational as stored, never with a zero denominator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignedRational {
    pub num: i32,
    pub den: i32,
}

impl SignedRational {
    pub fn value(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }
}

/// A position in degrees, south and west negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpsPosition {
    pub latitude: f64,
    pub longitude: f64,
    /// Metres above sea level, negative below it.
    pub altitude: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// How an embedded thumbnail's bytes are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThumbnailFormat {
    /// A JPEG stream from SOI to EOI; its pixel size when the container states it.
    Jpeg { size: Option<Dimensions> },
    /// Uncompressed interleaved 8-bit RGB, row by row, `size.width * size.height * 3` bytes.
    Rgb8 { size: Dimensions },
}

/// Where the small thumbnail beside the metadata lies in the file: its bytes are
/// `offset..offset + length`, inside the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Thumbnail {
    pub offset: u64,
    pub length: u64,
    pub format: ThumbnailFormat,
}

/// A photo file's header: the typed capture metadata indexing keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct FileHeader {
    pub container: Container,
    /// DateTimeOriginal, else DateTimeDigitized, else IFD0's DateTime, each with its own paired
    /// subsecond and offset tags.
    pub capture_time: Option<CaptureTime>,
    pub gps_time: Option<GpsTime>,
    pub make: Option<String>,
    pub model: Option<String>,
    /// EXIF BodySerialNumber, else DNG CameraSerialNumber, else the maker note's plain serial
    /// (Nikon, Canon, Fujifilm, Olympus and OM System, Pentax, Panasonic).
    pub body_serial: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub exposure_time: Option<Rational>,
    pub f_number: Option<Rational>,
    pub iso: Option<u32>,
    pub exposure_bias: Option<SignedRational>,
    pub focal_length: Option<Rational>,
    pub focal_length_35mm: Option<u32>,
    pub gps: Option<GpsPosition>,
    /// The full-resolution image's size as the container declares it, before orientation, cropped
    /// to the container's own default crop when it declares one; never a preview's.
    pub dimensions: Option<Dimensions>,
    /// EXIF orientation 1 to 8.
    pub orientation: Option<u8>,
    /// The smallest embedded JPEG thumbnail, else an uncompressed RGB one.
    pub thumbnail: Option<Thumbnail>,
}

impl FileHeader {
    /// The header of a whole file in memory: the same parsers as [`read_header`], unbounded.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        container::read(&mut source::Whole(bytes))
    }

    /// The exposure time in seconds.
    pub fn exposure_seconds(&self) -> Option<f64> {
        self.exposure_time.map(Rational::value)
    }
}

/// A bounded header read and what it cost.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderRead {
    pub header: FileHeader,
    /// Bytes read from the file, head included.
    pub bytes_read: u64,
    /// Reads made, head included.
    pub reads: u32,
    /// Whether the budget stopped a read, so the header may miss fields the whole file has.
    pub capped: bool,
}

/// Read the header of `file`, which is `len` bytes long, within the budget above.
///
/// Returns `io::Result` rather than the crate's error so that the caller, which knows the path,
/// words the error and can tell a file that vanished (`NotFound`) from one it cannot read.
pub fn read_header<R: Read + Seek>(file: &mut R, len: u64) -> io::Result<HeaderRead> {
    let mut source = source::Bounded::new(file, len);
    let header = container::read(&mut source);
    let (bytes_read, reads, capped, error) = source.finish();
    match error {
        Some(error) => Err(error),
        None => Ok(HeaderRead {
            header,
            bytes_read,
            reads,
            capped,
        }),
    }
}
