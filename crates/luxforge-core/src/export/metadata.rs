//! The original's capture metadata and the export's EXIF segment.
//!
//! Contract (`docs/design/export.md#keep-metadata`):
//! - `CaptureMetadata::from_jpeg(bytes)` reads the APP1 EXIF of a whole JPEG file;
//!   `CaptureMetadata::from_raw(bytes)` reads a TIFF-structured RAW (NEF, DNG) or the EXIF of the
//!   JPEG a RAF embeds. Neither ever fails: malformed or absent EXIF is empty metadata.
//! - `exif_payload(width, height)` is the TIFF payload of the export's APP1 segment, without the
//!   `Exif\0\0` header: the kept fields plus Orientation 1, ColorSpace sRGB, PixelXDimension,
//!   PixelYDimension, ExifVersion and Software.
//! - `field_names()` lists the kept fields by their EXIF names, in a stable order.

/// The whitelisted, validated EXIF fields of one original. Holds no floats, so it is `Eq`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureMetadata {}

impl CaptureMetadata {
    pub fn from_jpeg(_bytes: &[u8]) -> Self {
        Self::default()
    }

    pub fn from_raw(_bytes: &[u8]) -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        true
    }

    pub fn field_names(&self) -> Vec<&'static str> {
        Vec::new()
    }

    pub fn exif_payload(&self, _width: u32, _height: u32) -> Vec<u8> {
        unimplemented!("export metadata writer")
    }
}
