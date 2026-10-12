//! What an export asks its renderer for (`docs/design/export-settings.md`, rendering): the output's
//! pixel size, which is the output stage's or smaller, never larger, and the bits per channel of its
//! encoding. The GPU's bands and the reference's frame are both of this shape, and the encoders read
//! it, so a 16-bit output is encoded once at 16 bits rather than widened from 8.

/// Bits per channel of an export's output encoding: 8 for every format, 16 for TIFF only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Depth {
    #[default]
    Eight,
    Sixteen,
}

impl Depth {
    /// Bytes per channel sample.
    pub const fn bytes(self) -> usize {
        match self {
            Self::Eight => 1,
            Self::Sixteen => 2,
        }
    }
}

/// The pixels an export renders: `width` × `height`, at most the output stage's size on both sides
/// and scaled from it by one factor, encoded at `depth`. A `width` and `height` equal to the stage's
/// are the full-size output, which is rendered without resampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub depth: Depth,
}

impl OutputSpec {
    /// The full-size 8-bit output of a `width` × `height` stage: today's export.
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            depth: Depth::Eight,
        }
    }

    /// Whether this output is the stage's own size, so nothing is resampled.
    pub const fn is_full(&self, stage_width: u32, stage_height: u32) -> bool {
        self.width == stage_width && self.height == stage_height
    }
}

/// Opaque RGBA rows of an output, row by row from the left: 8-bit sRGB codes, or 16-bit sRGB codes
/// (0–65,535, alpha 65,535) in native byte order. Each row is `width × 4` samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pixels {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

impl Pixels {
    pub fn depth(&self) -> Depth {
        match self {
            Self::Eight(_) => Depth::Eight,
            Self::Sixteen(_) => Depth::Sixteen,
        }
    }

    /// Borrowed rows, as an encoder reads them.
    pub fn rows(&self) -> PixelRows<'_> {
        match self {
            Self::Eight(samples) => PixelRows::Eight(samples),
            Self::Sixteen(samples) => PixelRows::Sixteen(samples),
        }
    }
}

/// [`Pixels`] borrowed: what an encoder is handed, a band or a whole frame, without a copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelRows<'a> {
    Eight(&'a [u8]),
    Sixteen(&'a [u16]),
}

impl PixelRows<'_> {
    pub fn depth(&self) -> Depth {
        match self {
            Self::Eight(_) => Depth::Eight,
            Self::Sixteen(_) => Depth::Sixteen,
        }
    }

    /// Channel samples held, four per pixel.
    pub fn len(&self) -> usize {
        match self {
            Self::Eight(samples) => samples.len(),
            Self::Sixteen(samples) => samples.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
