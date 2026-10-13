//! An export's settings (`docs/design/export-settings.md`, settings): one [`ExportSettings`] shared
//! by `export.plan`, `export.write`, `batch.export`, the export presets and the remembered last
//! export, with the output-size arithmetic, the encoder's held-memory estimate and the plan's
//! notices that follow from it.
//!
//! The JSON shape is flat and exact. Every field that applies to the chosen `format` is required,
//! a nullable one (`limit_bytes`, `sharpen`, `pixels_per_inch`) as `null` when it is unset; every
//! field that does not apply is left out, and sending one, even as `null`, is refused with
//! `validation` naming it. So a JPEG has `quality`, `chroma` and `limit_bytes` and no `bit_depth`,
//! a WebP `quality` and `limit_bytes`, and a TIFF `bit_depth` and none of the three. Each refusal
//! names its field as `settings.<field>`, in the message and in `data.field`.
//!
//! Nothing here touches the file system: whether a chosen folder exists is the caller's to check,
//! once, where it plans or writes.
use crate::{
    Error,
    tiles::{
        output::Depth,
        sharpen::{OutputSharpen, Sharpen, SharpenAmount, SharpenMedium},
    },
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The smallest and largest bound a size names, in pixels.
pub const SIZE_BOUND: std::ops::RangeInclusive<u32> = 16..=65_535;
/// The quality range of JPEG and WebP.
pub const QUALITY_RANGE: std::ops::RangeInclusive<u8> = 1..=100;
/// The smallest and largest file-size limit, in bytes: 10 KiB to 1 GiB.
pub const LIMIT_RANGE: std::ops::RangeInclusive<u64> = 10_240..=1_073_741_824;
/// The densities a file may name, in pixels per inch.
pub const DENSITY_RANGE: std::ops::RangeInclusive<u16> = 1..=65_535;
/// The longest suffix, in characters.
pub const MAX_SUFFIX: usize = 64;
/// The longest chosen folder, in bytes, as any host path parameter.
pub const MAX_FOLDER_BYTES: usize = 4096;

/// The output above which an optimised or size-limited export warns (`large-image`): 24 MP.
pub const LARGE_IMAGE_PIXELS: u64 = 24_000_000;
/// The output frame a size limit and a WebP hold, per pixel: 8-bit RGBA.
pub const FRAME_BYTES: u64 = 4;

/// The file format an export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Jpeg,
    Webp,
    Tiff,
}

impl ExportFormat {
    pub const ALL: [Self; 3] = [Self::Jpeg, Self::Webp, Self::Tiff];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Tiff => "tiff",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|format| format.as_str() == value)
    }

    /// The extension a suggested name takes.
    pub fn extension(self) -> &'static str {
        self.extensions()[0]
    }

    /// Every extension a destination of this format may end in, compared ignoring case.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Jpeg => &["jpg", "jpeg"],
            Self::Webp => &["webp"],
            Self::Tiff => &["tif", "tiff"],
        }
    }
}

/// How large the output is against the output stage. Each bound is within [`SIZE_BOUND`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutputSize {
    /// The stage's own size.
    Full,
    /// The longer side at most `pixels`.
    LongEdge { pixels: u32 },
    /// Within `width` × `height`.
    Within { width: u32, height: u32 },
}

impl OutputSize {
    /// The output of a `width` × `height` stage: both sides scaled by one factor, never above 1,
    /// and rounded to whole pixels, half up, each at least 1. A stage already within the bound
    /// keeps its size. The factor is exact: a ratio of whole numbers, never a float.
    pub fn apply(self, (width, height): (u32, u32)) -> (u32, u32) {
        // The factor as `numerator / denominator`.
        let (numerator, denominator) = match self {
            Self::Full => return (width, height),
            Self::LongEdge { pixels } => (u64::from(pixels), u64::from(width.max(height))),
            Self::Within {
                width: bound_width,
                height: bound_height,
            } => {
                // The smaller of the two ratios, compared by cross-multiplying.
                let by_width = (u64::from(bound_width), u64::from(width));
                let by_height = (u64::from(bound_height), u64::from(height));
                if by_width.0 * by_height.1 <= by_height.0 * by_width.1 {
                    by_width
                } else {
                    by_height
                }
            }
        };
        if denominator == 0 || numerator >= denominator {
            return (width, height);
        }
        let scale = |side: u32| {
            let scaled = (u64::from(side) * numerator * 2 + denominator) / (2 * denominator);
            scaled.max(1) as u32
        };
        (scale(width), scale(height))
    }

    fn json(self) -> Value {
        match self {
            Self::Full => json!({"fit": "full"}),
            Self::LongEdge { pixels } => json!({"fit": "long_edge", "pixels": pixels}),
            Self::Within { width, height } => {
                json!({"fit": "within", "width": width, "height": height})
            }
        }
    }

    fn bounds(self) -> Vec<(&'static str, u32)> {
        match self {
            Self::Full => Vec::new(),
            Self::LongEdge { pixels } => vec![("pixels", pixels)],
            Self::Within { width, height } => vec![("width", width), ("height", height)],
        }
    }
}

/// A JPEG's chroma sampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chroma {
    /// 4:4:4, full.
    Full,
    /// 4:2:0, half.
    Half,
}

impl Chroma {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "444",
            Self::Half => "420",
        }
    }
}

/// Which of the original's supported EXIF fields an export writes (`docs/design/export-settings.md`,
/// metadata).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetadataLevel {
    /// No EXIF at all.
    None,
    /// Artist and Copyright.
    Copyright,
    /// Every supported field but the GPS group.
    NoLocation,
    /// Every supported field.
    All,
}

impl MetadataLevel {
    pub const ALL: [Self; 4] = [Self::None, Self::Copyright, Self::NoLocation, Self::All];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Copyright => "copyright",
            Self::NoLocation => "no_location",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == value)
    }

    /// Whether the export writes EXIF at all: every level but none, which also writes the
    /// structural fields.
    pub fn writes_exif(self) -> bool {
        self != Self::None
    }
}

/// Where an export is saved.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ExportFolder {
    /// The folder of the photograph's original.
    Original,
    /// A chosen folder: an absolute path, which must be an existing directory when the export
    /// plans or writes.
    Path(PathBuf),
}

impl ExportFolder {
    /// The folder an export of a photograph whose original is `original` is saved in, when there
    /// is one: a derived photograph has no original folder.
    pub fn resolve(&self, original: Option<&Path>) -> Option<PathBuf> {
        match self {
            Self::Original => original
                .and_then(Path::parent)
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf),
            Self::Path(path) => Some(path.clone()),
        }
    }
}

/// What a format's own fields hold. Each format carries exactly the fields that apply to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormatSettings {
    Jpeg {
        quality: u8,
        chroma: Chroma,
        limit_bytes: Option<u64>,
    },
    Webp {
        quality: u8,
        limit_bytes: Option<u64>,
    },
    Tiff {
        bit_depth: Depth,
    },
}

/// One export's settings. Build it from JSON ([`ExportSettings::from_json`] or serde), or in code
/// and then [`ExportSettings::check`] it; every caller that accepts settings checks them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExportSettings {
    pub format: FormatSettings,
    pub size: OutputSize,
    /// The format's slower, smaller encoding: progressive, optimised tables and trellis for JPEG,
    /// the slowest method for WebP, lossless Deflate for TIFF.
    pub optimise: bool,
    /// Output sharpening after resizing, if any ([`crate::tiles::sharpen`]).
    pub sharpen: Option<Sharpen>,
    pub metadata: MetadataLevel,
    /// The density the file names, if any.
    pub pixels_per_inch: Option<u16>,
    pub folder: ExportFolder,
    /// Added to the photograph's stem in the file's name.
    pub suffix: String,
}

/// Every settings field, in the order the JSON shape writes them.
const FIELDS: [&str; 12] = [
    "format",
    "size",
    "quality",
    "optimise",
    "chroma",
    "limit_bytes",
    "bit_depth",
    "sharpen",
    "metadata",
    "pixels_per_inch",
    "folder",
    "suffix",
];

/// Whether `field` applies to `format`.
fn applies(field: &str, format: ExportFormat) -> bool {
    match field {
        "quality" | "limit_bytes" => format != ExportFormat::Tiff,
        "chroma" => format == ExportFormat::Jpeg,
        "bit_depth" => format == ExportFormat::Tiff,
        _ => true,
    }
}

/// A refusal naming `field` within the settings.
fn refused(field: &str, reason: impl std::fmt::Display) -> Error {
    Error::validation(format!("settings.{field} {reason}"))
        .with_data(json!({"field": format!("settings.{field}")}))
}

impl ExportSettings {
    /// Today's export: a full-size quality-90 JPEG at 4:4:4, streamed without optimisation or a
    /// limit, beside the original with no suffix, writing EXIF at `metadata`.
    pub fn baseline(metadata: MetadataLevel) -> Self {
        Self {
            format: FormatSettings::Jpeg {
                quality: 90,
                chroma: Chroma::Full,
                limit_bytes: None,
            },
            size: OutputSize::Full,
            optimise: false,
            sharpen: None,
            metadata,
            pixels_per_inch: None,
            folder: ExportFolder::Original,
            suffix: String::new(),
        }
    }

    pub fn format(&self) -> ExportFormat {
        match self.format {
            FormatSettings::Jpeg { .. } => ExportFormat::Jpeg,
            FormatSettings::Webp { .. } => ExportFormat::Webp,
            FormatSettings::Tiff { .. } => ExportFormat::Tiff,
        }
    }

    /// JPEG's and WebP's quality.
    pub fn quality(&self) -> Option<u8> {
        match self.format {
            FormatSettings::Jpeg { quality, .. } | FormatSettings::Webp { quality, .. } => {
                Some(quality)
            }
            FormatSettings::Tiff { .. } => None,
        }
    }

    /// JPEG's chroma sampling.
    pub fn chroma(&self) -> Option<Chroma> {
        match self.format {
            FormatSettings::Jpeg { chroma, .. } => Some(chroma),
            _ => None,
        }
    }

    /// The file-size limit, JPEG and WebP only.
    pub fn limit_bytes(&self) -> Option<u64> {
        match self.format {
            FormatSettings::Jpeg { limit_bytes, .. } | FormatSettings::Webp { limit_bytes, .. } => {
                limit_bytes
            }
            FormatSettings::Tiff { .. } => None,
        }
    }

    /// The output encoding's bits per channel: 16 only for a 16-bit TIFF.
    pub fn depth(&self) -> Depth {
        match self.format {
            FormatSettings::Tiff { bit_depth } => bit_depth,
            _ => Depth::Eight,
        }
    }

    /// The output of a `width` × `height` stage ([`OutputSize::apply`]).
    pub fn output_size(&self, stage: (u32, u32)) -> (u32, u32) {
        self.size.apply(stage)
    }

    /// What the renderer is asked for: the output size at the output's depth, sharpened for the
    /// settings' medium at their density ([`OutputSharpen::new`]).
    pub fn output_spec(&self, stage: (u32, u32)) -> crate::tiles::output::OutputSpec {
        let (width, height) = self.output_size(stage);
        crate::tiles::output::OutputSpec {
            width,
            height,
            depth: self.depth(),
            sharpen: self
                .sharpen
                .map(|sharpen| OutputSharpen::new(sharpen, self.pixels_per_inch)),
        }
    }

    /// Every rule a value meets that its type cannot say, refused by the field's name: the same
    /// check for settings read from a request, a preset file or the remembered last export, and for
    /// settings built in code.
    pub fn check(&self) -> Result<(), Error> {
        for (name, bound) in self.size.bounds() {
            if !SIZE_BOUND.contains(&bound) {
                return Err(refused(
                    "size",
                    format_args!(
                        "{name} must be an integer from {} to {} pixels",
                        SIZE_BOUND.start(),
                        SIZE_BOUND.end()
                    ),
                ));
            }
        }
        if let Some(quality) = self.quality()
            && !QUALITY_RANGE.contains(&quality)
        {
            return Err(refused("quality", "must be an integer from 1 to 100"));
        }
        if let Some(limit) = self.limit_bytes()
            && !LIMIT_RANGE.contains(&limit)
        {
            return Err(refused(
                "limit_bytes",
                format_args!(
                    "must be null or an integer from {} to {} bytes",
                    LIMIT_RANGE.start(),
                    LIMIT_RANGE.end()
                ),
            ));
        }
        if let Some(density) = self.pixels_per_inch
            && !DENSITY_RANGE.contains(&density)
        {
            return Err(refused(
                "pixels_per_inch",
                "must be null or an integer from 1 to 65535",
            ));
        }
        if let ExportFolder::Path(path) = &self.folder {
            if !path.is_absolute() {
                return Err(refused(
                    "folder",
                    "must be \"original\" or {path} with an absolute path",
                ));
            }
            if path.as_os_str().len() > MAX_FOLDER_BYTES {
                return Err(refused(
                    "folder",
                    format_args!("path must be at most {MAX_FOLDER_BYTES} bytes"),
                ));
            }
        }
        check_suffix(&self.suffix)
    }

    /// The settings a JSON value describes, checked ([`Self::check`]). Every refusal is
    /// `validation` naming the field.
    pub fn from_json(value: &Value) -> Result<Self, Error> {
        let Value::Object(object) = value else {
            return Err(Error::validation("settings must be an object")
                .with_data(json!({"field": "settings"})));
        };
        if let Some(unknown) = object.keys().find(|key| !FIELDS.contains(&key.as_str())) {
            return Err(refused(unknown, "is not an export setting"));
        }
        let format = match object.get("format") {
            None => return Err(refused("format", "is required")),
            Some(value) => value
                .as_str()
                .and_then(ExportFormat::parse)
                .ok_or_else(|| refused("format", "must be jpeg, webp or tiff"))?,
        };
        for field in FIELDS {
            match (applies(field, format), object.contains_key(field)) {
                (true, false) => {
                    return Err(refused(
                        field,
                        format_args!("is required for {}", format.as_str()),
                    ));
                }
                (false, true) => {
                    return Err(refused(
                        field,
                        format_args!("does not apply to {}; leave it out", format.as_str()),
                    ));
                }
                _ => {}
            }
        }
        let field = |name: &str| &object[name];
        let quality = || -> Result<u8, Error> {
            field("quality")
                .as_u64()
                .and_then(|quality| u8::try_from(quality).ok())
                .filter(|quality| QUALITY_RANGE.contains(quality))
                .ok_or_else(|| refused("quality", "must be an integer from 1 to 100"))
        };
        let limit_bytes = || -> Result<Option<u64>, Error> {
            match field("limit_bytes") {
                Value::Null => Ok(None),
                value => value
                    .as_u64()
                    .filter(|limit| LIMIT_RANGE.contains(limit))
                    .map(Some)
                    .ok_or_else(|| {
                        refused(
                            "limit_bytes",
                            format_args!(
                                "must be null or an integer from {} to {} bytes",
                                LIMIT_RANGE.start(),
                                LIMIT_RANGE.end()
                            ),
                        )
                    }),
            }
        };
        let format = match format {
            ExportFormat::Jpeg => FormatSettings::Jpeg {
                quality: quality()?,
                chroma: match field("chroma").as_str() {
                    Some("444") => Chroma::Full,
                    Some("420") => Chroma::Half,
                    _ => return Err(refused("chroma", "must be \"444\" or \"420\"")),
                },
                limit_bytes: limit_bytes()?,
            },
            ExportFormat::Webp => FormatSettings::Webp {
                quality: quality()?,
                limit_bytes: limit_bytes()?,
            },
            ExportFormat::Tiff => FormatSettings::Tiff {
                bit_depth: match field("bit_depth").as_u64() {
                    Some(8) => Depth::Eight,
                    Some(16) => Depth::Sixteen,
                    _ => return Err(refused("bit_depth", "must be 8 or 16")),
                },
            },
        };
        let settings = Self {
            format,
            size: parse_size(field("size"))?,
            optimise: field("optimise")
                .as_bool()
                .ok_or_else(|| refused("optimise", "must be true or false"))?,
            sharpen: parse_sharpen(field("sharpen"))?,
            metadata: field("metadata")
                .as_str()
                .and_then(MetadataLevel::parse)
                .ok_or_else(|| {
                    refused("metadata", "must be none, copyright, no_location or all")
                })?,
            pixels_per_inch: match field("pixels_per_inch") {
                Value::Null => None,
                value => Some(
                    value
                        .as_u64()
                        .and_then(|density| u16::try_from(density).ok())
                        .filter(|density| DENSITY_RANGE.contains(density))
                        .ok_or_else(|| {
                            refused(
                                "pixels_per_inch",
                                "must be null or an integer from 1 to 65535",
                            )
                        })?,
                ),
            },
            folder: parse_folder(field("folder"))?,
            suffix: field("suffix")
                .as_str()
                .ok_or_else(|| refused("suffix", "must be a string"))?
                .to_owned(),
        };
        settings.check()?;
        Ok(settings)
    }

    /// The JSON shape: every field that applies to the format, nullable ones as `null` when
    /// unset, and none that does not.
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("format".into(), json!(self.format().as_str()));
        object.insert("size".into(), self.size.json());
        match self.format {
            FormatSettings::Jpeg {
                quality,
                chroma,
                limit_bytes,
            } => {
                object.insert("quality".into(), json!(quality));
                object.insert("optimise".into(), json!(self.optimise));
                object.insert("chroma".into(), json!(chroma.as_str()));
                object.insert("limit_bytes".into(), json!(limit_bytes));
            }
            FormatSettings::Webp {
                quality,
                limit_bytes,
            } => {
                object.insert("quality".into(), json!(quality));
                object.insert("optimise".into(), json!(self.optimise));
                object.insert("limit_bytes".into(), json!(limit_bytes));
            }
            FormatSettings::Tiff { bit_depth } => {
                object.insert("optimise".into(), json!(self.optimise));
                object.insert(
                    "bit_depth".into(),
                    json!(match bit_depth {
                        Depth::Eight => 8,
                        Depth::Sixteen => 16,
                    }),
                );
            }
        }
        object.insert(
            "sharpen".into(),
            self.sharpen.map_or(Value::Null, |sharpen| {
                json!({"medium": sharpen.medium.as_str(), "amount": sharpen.amount.as_str()})
            }),
        );
        object.insert("metadata".into(), json!(self.metadata.as_str()));
        object.insert("pixels_per_inch".into(), json!(self.pixels_per_inch));
        object.insert(
            "folder".into(),
            match &self.folder {
                ExportFolder::Original => json!("original"),
                ExportFolder::Path(path) => json!({"path": path}),
            },
        );
        object.insert("suffix".into(), json!(self.suffix));
        Value::Object(object)
    }

    /// The bytes the encoder holds beyond the streamed bands, for an output of `width` × `height`,
    /// from the encoders' own figures: an optimised JPEG's coefficient arrays
    /// ([`encoder_bytes`](super::encode::encoder_bytes), about 12 bytes a pixel at 4:4:4 and 6 at
    /// 4:2:0); for a size-limited JPEG, the output frame at 4 bytes a pixel, one attempt's arrays
    /// and two files at most the limit, the attempt's and the best so far; for a WebP, the frame
    /// and libwebp's state with its file ([`webp_held_bytes`](super::webp::webp_held_bytes)), and,
    /// with a limit, the best file so far. A streamed JPEG or TIFF holds nothing more. The one
    /// place the figure is computed, so the plan, its notice and the sheet agree.
    pub fn held_bytes(&self, (width, height): (u32, u32)) -> u64 {
        let frame = u64::from(width) * u64::from(height) * FRAME_BYTES;
        match self.format {
            FormatSettings::Jpeg {
                quality,
                chroma,
                limit_bytes,
            } => {
                let options = super::encode::JpegOptions {
                    quality,
                    chroma,
                    optimise: self.optimise,
                };
                let coefficients = super::encode::encoder_bytes(width, height, &options);
                match limit_bytes {
                    None => coefficients,
                    Some(limit) => frame + coefficients + 2 * limit,
                }
            }
            FormatSettings::Webp { limit_bytes, .. } => {
                frame
                    + super::webp::webp_held_bytes(width, height, self.optimise)
                    + limit_bytes.unwrap_or(0)
            }
            FormatSettings::Tiff { .. } => 0,
        }
    }

    /// Whether the export holds memory that scales with its output: an optimised JPEG, any WebP
    /// (libwebp encodes a whole picture), or any size limit.
    fn holds_output(&self) -> bool {
        match self.format() {
            ExportFormat::Jpeg => self.optimise || self.limit_bytes().is_some(),
            ExportFormat::Webp => true,
            ExportFormat::Tiff => false,
        }
    }

    /// The notices a plan of an output of `width` × `height` carries, in order: `large-image` when
    /// an output that holds memory with its size (an optimised JPEG, a WebP or a size limit) passes
    /// [`LARGE_IMAGE_PIXELS`], with the estimate, which warns and never refuses (owner,
    /// 2026-10-08 and 2026-10-13: no memory cap); `webp-too-large` when a WebP side passes the
    /// format's [`WEBP_MAX_SIDE`]; `tiff-too-large` when a TIFF could pass the format's 4 GiB;
    /// `folder-missing` when `folder` is not an existing directory, or is `None` because the
    /// photograph has no original folder.
    pub fn notices(&self, output: (u32, u32), folder: Option<&Path>) -> Vec<Notice> {
        let (width, height) = output;
        let pixels = u64::from(width) * u64::from(height);
        let mut notices = Vec::new();
        if self.holds_output() && pixels > LARGE_IMAGE_PIXELS {
            let bytes = self.held_bytes(output);
            let megapixels = pixels as f64 / 1_000_000.0;
            let megabytes = bytes.div_ceil(1_000_000);
            let message = match (self.format(), self.optimise, self.limit_bytes()) {
                (ExportFormat::Webp, false, None) => format!(
                    "Large image: WebP holds the whole {megapixels:.1} MP output while it \
                     encodes, about {megabytes} MB"
                ),
                (_, true, None) => format!(
                    "Large image: optimising {megapixels:.1} MP holds about {megabytes} MB while \
                     encoding and is several times slower"
                ),
                _ => format!(
                    "Large image: limiting the file size of {megapixels:.1} MP holds about \
                     {megabytes} MB while encoding and is several times slower"
                ),
            };
            notices.push(Notice {
                code: LARGE_IMAGE,
                message,
                refuses: false,
                bytes: Some(bytes),
            });
        }
        if self.format() == ExportFormat::Webp && !super::webp::webp_fits(width, height) {
            notices.push(Notice {
                code: WEBP_TOO_LARGE,
                message: format!(
                    "WebP is limited to {WEBP_MAX_SIDE} px a side; this output is {width} \u{d7} \
                     {height}"
                ),
                refuses: true,
                bytes: None,
            });
        }
        if let FormatSettings::Tiff { bit_depth } = self.format
            && !super::tiff::tiff_fits(width, height, bit_depth)
        {
            notices.push(Notice {
                code: TIFF_TOO_LARGE,
                message: format!(
                    "a {width} \u{d7} {height} {}-bit TIFF could pass the format's 4 GiB limit; \
                     choose a smaller size or 8 bits",
                    bit_depth.bytes() * 8
                ),
                refuses: true,
                bytes: None,
            });
        }
        match folder {
            Some(folder) if folder.is_dir() => {}
            Some(folder) => notices.push(Notice {
                code: FOLDER_MISSING,
                message: format!("{} is not an existing folder", folder.display()),
                refuses: true,
                bytes: None,
            }),
            None => notices.push(Notice {
                code: FOLDER_MISSING,
                message: "this photograph has no original's folder; choose a folder".into(),
                refuses: true,
                bytes: None,
            }),
        }
        notices
    }
}

/// `large-image`: an optimised or size-limited output above 24 MP.
pub const LARGE_IMAGE: &str = "large-image";
/// `tiff-too-large`: a TIFF that could pass the format's 4 GiB. It refuses the export.
pub(crate) use super::tiff::TIFF_TOO_LARGE;
/// `webp-too-large`: a WebP output side above 16,383 px, the format's own limit. It refuses the
/// export.
pub(crate) use super::webp::{WEBP_MAX_SIDE, WEBP_TOO_LARGE};
/// `folder-missing`: the export's folder is not an existing directory. It refuses the export.
pub const FOLDER_MISSING: &str = "folder-missing";

/// One thing a plan says about an export before it runs: a code, its words, whether it refuses the
/// export (which `export.write` then refuses with the same code in `data.notice`), and the held
/// bytes a `large-image` notice warns of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub code: &'static str,
    pub message: String,
    pub refuses: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

impl Notice {
    /// The refusal `export.write` and `batch.export` answer for a notice that refuses the export.
    pub fn refusal(&self) -> Error {
        Error::validation(self.message.clone()).with_data(json!({"notice": self.code}))
    }
}

/// A suffix is 0–64 characters with no path separator, no control character and no leading dot.
fn check_suffix(suffix: &str) -> Result<(), Error> {
    if suffix.chars().count() > MAX_SUFFIX {
        return Err(refused(
            "suffix",
            format_args!("must be at most {MAX_SUFFIX} characters"),
        ));
    }
    if suffix.chars().any(|c| c == '/' || c == '\\') {
        return Err(refused("suffix", "must not contain a path separator"));
    }
    if suffix.chars().any(char::is_control) {
        return Err(refused("suffix", "must not contain a control character"));
    }
    if suffix.starts_with('.') {
        return Err(refused("suffix", "must not start with a dot"));
    }
    Ok(())
}

fn parse_size(value: &Value) -> Result<OutputSize, Error> {
    let shape = || {
        refused(
            "size",
            "must be {fit: full}, {fit: long_edge, pixels} or {fit: within, width, height}",
        )
    };
    let object = value.as_object().ok_or_else(shape)?;
    let bound = |name: &str| -> Result<u32, Error> {
        object
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|bound| u32::try_from(bound).ok())
            .filter(|bound| SIZE_BOUND.contains(bound))
            .ok_or_else(|| {
                refused(
                    "size",
                    format_args!(
                        "{name} must be an integer from {} to {} pixels",
                        SIZE_BOUND.start(),
                        SIZE_BOUND.end()
                    ),
                )
            })
    };
    let (size, keys): (OutputSize, &[&str]) = match object.get("fit").and_then(Value::as_str) {
        Some("full") => (OutputSize::Full, &["fit"]),
        Some("long_edge") => (
            OutputSize::LongEdge {
                pixels: bound("pixels")?,
            },
            &["fit", "pixels"],
        ),
        Some("within") => (
            OutputSize::Within {
                width: bound("width")?,
                height: bound("height")?,
            },
            &["fit", "width", "height"],
        ),
        _ => return Err(shape()),
    };
    if object.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err(shape());
    }
    Ok(size)
}

fn parse_sharpen(value: &Value) -> Result<Option<Sharpen>, Error> {
    let shape = || {
        refused(
            "sharpen",
            "must be null or {medium: screen, matte or glossy, amount: low, standard or high}",
        )
    };
    match value {
        Value::Null => Ok(None),
        Value::Object(object) if object.len() == 2 => {
            let medium = object
                .get("medium")
                .and_then(Value::as_str)
                .and_then(SharpenMedium::parse)
                .ok_or_else(shape)?;
            let amount = object
                .get("amount")
                .and_then(Value::as_str)
                .and_then(SharpenAmount::parse)
                .ok_or_else(shape)?;
            Ok(Some(Sharpen { medium, amount }))
        }
        _ => Err(shape()),
    }
}

fn parse_folder(value: &Value) -> Result<ExportFolder, Error> {
    let shape = || {
        refused(
            "folder",
            "must be \"original\" or {path} with an absolute path",
        )
    };
    match value {
        Value::String(original) if original == "original" => Ok(ExportFolder::Original),
        Value::Object(object) if object.len() == 1 => object
            .get("path")
            .and_then(Value::as_str)
            .map(|path| ExportFolder::Path(PathBuf::from(path)))
            .ok_or_else(shape),
        _ => Err(shape()),
    }
}

impl Serialize for ExportSettings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExportSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_json(&value).map_err(|error| serde::de::Error::custom(error.detail))
    }
}

/// What `schema.list` publishes once for every method that takes settings.
pub(crate) fn schema() -> Value {
    json!({
        "fields": {
            "format": {"values": ExportFormat::ALL.map(ExportFormat::as_str), "applies": "all"},
            "size": {"values": "{fit: full}, {fit: long_edge, pixels} or {fit: within, width, height}", "bound": [SIZE_BOUND.start(), SIZE_BOUND.end()], "applies": "all"},
            "quality": {"range": [QUALITY_RANGE.start(), QUALITY_RANGE.end()], "applies": ["jpeg", "webp"]},
            "optimise": {"values": "boolean", "applies": "all", "notes": "jpeg: progressive scans, optimised Huffman tables and trellis quantisation with the standard tables; webp: the slowest, best method; tiff: lossless Deflate with the horizontal predictor"},
            "chroma": {"values": ["444", "420"], "applies": ["jpeg"]},
            "limit_bytes": {"range": [LIMIT_RANGE.start(), LIMIT_RANGE.end()], "nullable": true, "applies": ["jpeg", "webp"]},
            "bit_depth": {"values": [8, 16], "applies": ["tiff"]},
            "sharpen": {"values": {"medium": SharpenMedium::ALL.map(SharpenMedium::as_str), "amount": SharpenAmount::ALL.map(SharpenAmount::as_str)}, "nullable": true, "applies": "all", "notes": "output sharpening, null for none: {medium, amount}, applied after resizing at the output's size to luminance only, on the encoded codes, adding no colour fringes; screen sharpens with a small fixed radius, matte and glossy with one from pixels_per_inch (300 ppi when it is null), matte the stronger; low, standard and high scale the amount"},
            "metadata": {"values": MetadataLevel::ALL.map(MetadataLevel::as_str), "applies": "all", "notes": "none: no EXIF; copyright: Artist and Copyright; no_location: every supported field but the GPS group; all: every supported field; every level but none also writes Orientation 1, ColorSpace sRGB, the output's pixel dimensions, ExifVersion and Software"},
            "pixels_per_inch": {"range": [DENSITY_RANGE.start(), DENSITY_RANGE.end()], "nullable": true, "applies": "all"},
            "folder": {"values": "\"original\" (each photograph's own original's folder) or {path}, an absolute path to an existing folder", "applies": "all"},
            "suffix": {"max_characters": MAX_SUFFIX, "applies": "all", "notes": "no path separator, no control character and no leading dot"},
        },
        "notes": "ExportSettings, one flat JSON object. Every field that applies to the chosen format is required, a nullable one as null when it is unset; every field that does not apply is left out, and sending one, even as null, is validation naming it; so jpeg has quality, chroma and limit_bytes and no bit_depth, webp has quality and limit_bytes, and tiff has bit_depth and none of quality, chroma or limit_bytes. Every refusal is validation naming the field as settings.<field> in the message and in data.field. The output size scales the output stage, after orientation, transforms and crop, by one factor never above 1 and rounds each side to whole pixels, half up, at least 1, so an output is never enlarged. A file is named <original stem><suffix>.<extension> (jpg, webp or tif), counting up <stem><suffix>-2.<extension> and so on while a name is taken, reading at most 64 names; nothing is ever replaced.",
    })
}

/// Today's export's settings as JSON, beside the original, for a test that sends them.
#[cfg(test)]
pub(crate) fn baseline_json() -> Value {
    ExportSettings::baseline(MetadataLevel::None).to_json()
}

/// [`baseline_json`] saving to `folder`, as a derived photograph, which has no original's
/// folder, must.
#[cfg(test)]
pub(crate) fn baseline_json_in(folder: &Path) -> Value {
    ExportSettings {
        folder: ExportFolder::Path(folder.to_path_buf()),
        ..ExportSettings::baseline(MetadataLevel::None)
    }
    .to_json()
}

#[cfg(test)]
mod tests;
