//! Previews: what the preview lane caches and answers, and the honest name of every
//! preview's pixels.
//!
//! For a file, previews are keyed by its signature: the grid tier from its EXIF thumbnail or
//! embedded preview, and the loupe tier from its largest embedded preview. For a developed
//! photograph, by asset, entry and tier, rendered from that entry at full resolution by the owner's
//! tile service (the GPU on the desktop) or the reference renderer, and area-averaged to the tier. Files live under `<catalog>.index/previews/`; the index database's `previews` and `photo_previews` tables
//! record them. The lane never touches the editor's one-slot source cache.
use super::{Dimensions, FileId};
use crate::{AssetId, EntryId, JobId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The loupe tier's long edge at most, in pixels.
pub const LOUPE_MAX_SIDE: u32 = 2560;
/// A developed photograph's grid tier's long edge, in pixels.
pub const PHOTO_GRID_SIDE: u32 = 512;
/// A developed photograph's large tier's long edge, in pixels: Develop's instant switch.
pub const PHOTO_LARGE_SIDE: u32 = 2048;
/// The byte budget the loupe and large tiers share on disk, least recently used first out (P8).
/// Grid tiers are kept.
pub const SHARED_PREVIEW_BUDGET_BYTES: u64 = 4 << 30;

/// A preview's size class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewTier {
    /// A grid cell: a file's EXIF thumbnail or embedded preview, or a photograph's 512 px render.
    Grid,
    /// A file's largest embedded preview, stored at up to [`LOUPE_MAX_SIDE`].
    Loupe,
    /// A photograph's 2048 px render, for Develop's instant switch.
    Large,
}

impl PreviewTier {
    pub const ALL: [Self; 3] = [Self::Grid, Self::Loupe, Self::Large];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::Loupe => "loupe",
            Self::Large => "large",
        }
    }
}

/// What a preview is of: a file of the index, or a developed photograph at one entry (its current
/// entry when a request names none).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PreviewItem {
    File {
        file_id: FileId,
    },
    Photo {
        asset_id: AssetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_id: Option<EntryId>,
    },
}

/// What a preview's pixels are, which every surface that shows one says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewOrigin {
    /// The file's tiny EXIF thumbnail (about 160 px).
    ExifThumbnail,
    /// The camera's embedded preview.
    Embedded,
    /// A neutral Luxforge development of the frame, made where the camera's preview is too small
    /// or absent.
    Developed,
    /// A developed photograph's entry, rendered at full resolution by the owner's tile service or
    /// the reference renderer and area-averaged to the tier; its preview names which
    /// ([`PreviewInfo::renderer`]).
    Rendered,
}

impl PreviewOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExifThumbnail => "exif-thumbnail",
            Self::Embedded => "embedded",
            Self::Developed => "developed",
            Self::Rendered => "rendered",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::ExifThumbnail,
            Self::Embedded,
            Self::Developed,
            Self::Rendered,
        ]
        .into_iter()
        .find(|origin| origin.as_str() == value)
    }

    /// What a person reads beside it: "Camera preview", "Luxforge development", "Preview".
    pub fn label(self) -> &'static str {
        match self {
            Self::ExifThumbnail | Self::Embedded => "Camera preview",
            Self::Developed => "Luxforge development",
            Self::Rendered => "Preview",
        }
    }
}

/// Which renderer drew a file Luxforge rendered — a developed photograph's rendered tier, a batch
/// export's file — in the shape an `export.jpeg` result names its renderer: `{record: "gpu",
/// reason: null}`, or `{record: "reference", reason}` with the reason the GPU did not draw it (the
/// session's, an export's such as `refused` or `tiles-budget`, or the GPU plan's own code), and no
/// reason on an owner with no GPU provider, whose only renderer is the reference. Read back as
/// written: unlike a session's [`Renderer`](crate::Renderer), any reason is kept as its code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderedBy {
    pub record: crate::RendererRecord,
    pub reason: Option<String>,
}

impl RenderedBy {
    /// Drawn by the GPU.
    pub fn gpu() -> Self {
        Self {
            record: crate::RendererRecord::Gpu,
            reason: None,
        }
    }
}

impl From<crate::Renderer> for RenderedBy {
    fn from(renderer: crate::Renderer) -> Self {
        Self {
            record: renderer.record(),
            reason: renderer.reason().map(|reason| reason.as_str().to_owned()),
        }
    }
}

/// A cached preview: where its JPEG is, its size, what it is, whether it is an approximation, and
/// how many bytes it holds. `key` names exactly what it was made from (the file's signature, or the
/// asset, entry and renderer generation), so a client never shows it for anything else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewInfo {
    pub item: PreviewItem,
    pub tier: PreviewTier,
    /// An absolute path under `<catalog>.index/previews/`.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub origin: PreviewOrigin,
    /// Whether the pixels approximate the entry they are labelled with: never, since a rendered
    /// tier is its entry's full-resolution picture area-averaged, and a file's tiers and a camera
    /// preview are what they are labelled. Always present, so every preview says so.
    pub approximate: bool,
    pub bytes: u64,
    pub key: String,
    /// Which renderer drew a developed photograph's rendered tier: the GPU through the owner's
    /// tile service, or the reference naming why. Absent for a file's tiers and a camera preview,
    /// which no renderer drew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<RenderedBy>,
}

/// What `preview.read` answers: the cached preview, or the job making it and, meanwhile, the best
/// preview already cached (a grid tier or a thumbnail while the loupe tier is read).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PreviewAnswer {
    Ready {
        preview: PreviewInfo,
    },
    Queued {
        job_id: JobId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<PreviewInfo>,
    },
}

/// How urgently a preview is wanted: the loupe's look-ahead first, then visible grid cells, then
/// the rest of the view.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewPriority {
    Background,
    #[default]
    Visible,
    LookAhead,
}

impl PreviewPriority {
    pub const ALL: [Self; 3] = [Self::Background, Self::Visible, Self::LookAhead];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Visible => "visible",
            Self::LookAhead => "look-ahead",
        }
    }
}

/// A rectangle of an image's upright full-resolution pixels (after its orientation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A 100% region, as a `preview.region` job's result: the region's pixels in a JPEG the answer
/// names, valid until this client's next region, and what they are — the embedded full-size
/// preview, decoded only for the region, or a neutral development of the frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionAnswer {
    pub item: PreviewItem,
    /// The rectangle returned, in [`Self::frame`]: the request's, mapped into that frame and
    /// clamped to it.
    pub rect: PixelRect,
    /// The upright frame `rect` was cut from: the JPEG original's, the embedded preview's or the
    /// development's, each at full resolution.
    pub frame: Dimensions,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub origin: PreviewOrigin,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_answer_says_what_its_pixels_are() {
        let info = PreviewInfo {
            renderer: None,
            item: PreviewItem::File { file_id: FileId(4) },
            tier: PreviewTier::Grid,
            path: "/c.index/previews/ab/cd.jpg".into(),
            width: 160,
            height: 107,
            origin: PreviewOrigin::ExifThumbnail,
            approximate: false,
            bytes: 5120,
            key: "file:4:sig".into(),
        };
        let answer = PreviewAnswer::Ready {
            preview: info.clone(),
        };
        let json = serde_json::to_value(&answer).unwrap();
        assert_eq!(json["state"], "ready");
        assert_eq!(json["preview"]["origin"], "exif-thumbnail");
        assert_eq!(
            json["preview"]["approximate"], false,
            "serialized even when false"
        );
        assert_eq!(
            serde_json::from_value::<PreviewAnswer>(json.clone()).unwrap(),
            answer
        );
        let mut without = json["preview"].clone();
        without.as_object_mut().unwrap().remove("approximate");
        assert!(
            serde_json::from_value::<PreviewInfo>(without).is_err(),
            "every preview says whether it is approximate"
        );
        assert_eq!(
            json["preview"]["item"],
            json!({"kind": "file", "file_id": 4})
        );
        assert_eq!(info.origin.label(), "Camera preview");
        assert!(PreviewPriority::LookAhead > PreviewPriority::Visible);
        assert!(PreviewPriority::Visible > PreviewPriority::Background);
        for origin in [
            PreviewOrigin::ExifThumbnail,
            PreviewOrigin::Embedded,
            PreviewOrigin::Developed,
            PreviewOrigin::Rendered,
        ] {
            assert_eq!(PreviewOrigin::parse(origin.as_str()), Some(origin));
        }
    }

    /// A region names the rectangle it returns and the upright frame that rectangle is in.
    #[test]
    fn a_region_answer_names_its_rectangle_in_its_frame() {
        let answer = RegionAnswer {
            item: PreviewItem::File { file_id: FileId(4) },
            rect: PixelRect {
                x: 10,
                y: 20,
                width: 300,
                height: 200,
            },
            frame: Dimensions {
                width: 6048,
                height: 4024,
            },
            path: "/c.index/previews/regions/1-1.jpg".into(),
            width: 300,
            height: 200,
            origin: PreviewOrigin::Developed,
        };
        let json = serde_json::to_value(&answer).unwrap();
        assert_eq!(
            json["rect"],
            json!({"x": 10, "y": 20, "width": 300, "height": 200})
        );
        assert_eq!(json["frame"], json!({"width": 6048, "height": 4024}));
        assert_eq!(json["origin"], "developed");
        assert_eq!(
            serde_json::from_value::<RegionAnswer>(json).unwrap(),
            answer
        );
    }
}
