//! The fictional disk the generated index and catalog describe, the same for every run whatever
//! the output directory: three volumes, where each frame's file is on them, its signature and
//! what its header says. None of these files exists.
//!
//! - **Macintosh HD** (`/`): the pictures folder `/Users/generated/Pictures`, an indexed folder
//!   holding the September card dumps, the user-named folders, the phone's export, the undated
//!   files (`From Anna`, and the catalog's `Scans`) and the later trip folders
//!   (`<year>/<first day> <title>/`).
//! - **NIKON Z 8** (`/Volumes/NIKON Z 8`): the camera card, mounted and removable, holding both
//!   Z 8 bodies' September folders under `DCIM`.
//! - **Photos SSD** (`/Volumes/Photos SSD`): an external drive, not connected since the 23rd, whose
//!   archive folder (an indexed folder) holds the older trip folders.
use super::clock::{Date, HOUR, SECOND, offset_text};
use super::plan::{Drive, Event, Filing, Frame, PHONE};
use super::random::Random;
use crate::Result;
use luxforge_core::SourceTag;
use luxforge_core::catalog_types::{
    CameraBody, CaptureTime, Dimensions, EmbeddedFormat, EmbeddedImage, ExifOrientation, Exposure,
    FileIdentity, FileSignature, GeoPosition, HeaderMetadata, IndexRoot, IndexedFolder, RootKind,
    Volume, VolumeId,
};
use std::path::{Path, PathBuf};

/// The generated data's "now", 2026-09-30T10:00Z in milliseconds: when every online file was
/// last seen and every photograph's original last checked.
pub fn now_ms() -> i64 {
    Date::new(2026, 9, 30).at(10, 0).0
}

/// When the external drive was last connected, 2026-09-23T18:00Z.
fn external_seen_ms() -> i64 {
    Date::new(2026, 9, 23).at(18, 0).0
}

/// One of the three volumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disk {
    Internal,
    Card,
    External,
}

impl Disk {
    pub const ALL: [Disk; 3] = [Disk::Internal, Disk::Card, Disk::External];

    pub fn id(self) -> VolumeId {
        VolumeId::parse(match self {
            Disk::Internal => "volume-generated-macintosh-hd",
            Disk::Card => "volume-generated-nikon-z8-card",
            Disk::External => "volume-generated-photos-ssd",
        })
        .expect("a volume identity")
    }

    pub fn volume(self) -> Volume {
        let (mount_point, label, removable, platform_id) = match self {
            Disk::Internal => (
                "/",
                "Macintosh HD",
                false,
                "5F1C2A7E-0B6D-4C11-9A3E-7D2B8C4F1A01",
            ),
            Disk::Card => (
                "/Volumes/NIKON Z 8",
                "NIKON Z 8",
                true,
                "9C3B1D20-4E7F-4A2B-8D6C-1F0E2A3B4C02",
            ),
            Disk::External => (
                "/Volumes/Photos SSD",
                "Photos SSD",
                false,
                "2E8D4F6A-7B1C-4D3E-9F0A-5B6C7D8E9F03",
            ),
        };
        Volume {
            id: self.id(),
            mount_point: mount_point.into(),
            label: label.into(),
            removable,
            platform_id: Some(platform_id.into()),
            last_seen_ms: if self.online() {
                now_ms()
            } else {
                external_seen_ms()
            },
        }
    }

    /// Whether it is mounted now.
    pub fn online(self) -> bool {
        self != Disk::External
    }

    /// The folder the index lists on it, and why.
    pub fn root(self) -> (&'static str, RootKind) {
        match self {
            Disk::Internal => ("/Users/generated/Pictures", RootKind::Indexed),
            Disk::Card => ("/Volumes/NIKON Z 8", RootKind::Card),
            Disk::External => ("/Volumes/Photos SSD/Archive", RootKind::Indexed),
        }
    }

    /// The index's root on it, with the files a listing found.
    pub fn index_root(self, files: u32) -> IndexRoot {
        IndexRoot {
            path: self.root().0.into(),
            kind: self.root().1,
            volume_id: self.id(),
            listed_ms: Some(self.volume().last_seen_ms),
            file_count: Some(files),
            offline: !self.online(),
        }
    }

    /// Its folder in the catalog's list of indexed folders, when it is one.
    pub fn indexed_folder(self) -> Option<IndexedFolder> {
        (self.root().1 == RootKind::Indexed).then(|| IndexedFolder {
            path: self.root().0.into(),
            volume_id: self.id(),
            added_ms: Date::new(2026, 9, 1).at(9, 0).0,
            actor: "desktop".into(),
        })
    }

    fn device(self) -> u64 {
        match self {
            Disk::Internal => 16_777_232,
            Disk::Card => 16_777_240,
            Disk::External => 16_777_245,
        }
    }
}

/// One frame as a file on the fictional disk.
pub struct File {
    pub path: PathBuf,
    pub disk: Disk,
    pub kind: SourceTag,
    pub signature: FileSignature,
}

impl File {
    /// The folder it is in.
    pub fn folder(&self) -> &Path {
        self.path.parent().expect("a file in a folder")
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .expect("a file name")
            .to_string_lossy()
            .into_owned()
    }

    pub fn inode(&self) -> u64 {
        self.signature.identity.expect("an identity").inode
    }

    /// Its identity as the catalog stores it, `unix:<device>:<inode>`.
    pub fn identity_text(&self) -> String {
        let identity = self.signature.identity.expect("an identity");
        format!("unix:{}:{}", identity.device, identity.inode)
    }
}

/// Where `frame` of `event` is, as the `inode`th file of the run (a unique number).
pub fn locate(event: &Event, frame: &Frame, inode: u64) -> File {
    let name = event.file_name(frame);
    let (disk, folder) = match event.filing {
        Filing::BySource => match event.source_folder(frame) {
            (true, folder) => (Disk::Card, format!("{}/{folder}", Disk::Card.root().0)),
            (false, folder) => (
                Disk::Internal,
                format!("{}/{folder}", Disk::Internal.root().0),
            ),
        },
        Filing::Trip(drive) => {
            let disk = match drive {
                Drive::Internal => Disk::Internal,
                Drive::External => Disk::External,
            };
            let folder = if event.place.is_none() {
                format!("{}/{}", disk.root().0, event.folder)
            } else {
                format!("{}/{}/{}", disk.root().0, event.start.year, event.folder)
            };
            (disk, folder)
        }
    };
    let kind = if name.ends_with(".JPG") {
        SourceTag::Jpeg
    } else {
        SourceTag::Raw
    };
    let mut random = Random::stream(inode, 0x0046_494c_4553);
    let typical = if event.place.is_none() {
        2_400_000
    } else if event.jpeg && frame.body != PHONE {
        frame.body().bytes / 4
    } else {
        frame.body().bytes
    };
    let len = typical * random.between(90, 110) as u64 / 100;
    // A camera writes a file as it takes it and a copy keeps that time; an undated export was
    // written on the 20th.
    let modified_ms = match frame.utc() {
        Some(utc) => utc + random.between(300, 2500),
        None => Date::new(2026, 9, 20).at(14, 0).0 + inode as i64 * SECOND,
    };
    File {
        path: PathBuf::from(folder).join(name),
        disk,
        kind,
        signature: FileSignature {
            len,
            modified_ns: modified_ms * 1_000_000,
            identity: Some(FileIdentity {
                device: disk.device(),
                inode,
            }),
        },
    }
}

/// What `frame`'s header says, as the index reads it: from the same EXIF text the image folders
/// write, with the full-size image's dimensions and, for a file (`thumbnail`), where its embedded
/// 160 × 107 JPEG thumbnail is.
pub fn header(frame: &Frame, thumbnail: bool, inode: u64) -> Result<HeaderMetadata> {
    let body = frame.body();
    let exposure = frame.exposure;
    let offset = frame.written_offset().map(offset_text);
    Ok(HeaderMetadata {
        capture: frame.time.and_then(|time| {
            CaptureTime::from_exif(&time.exif(), Some(&time.subsec()), offset.as_deref())
        }),
        position: frame.gps.and_then(|gps| {
            GeoPosition::new(
                f64::from(gps.latitude) / 1e6,
                f64::from(gps.longitude) / 1e6,
                Some(f64::from(gps.altitude) / 10.0),
            )
        }),
        camera: Some(CameraBody {
            make: body.make.into(),
            model: body.model.into(),
            serial: body.serial.map(Into::into),
        }),
        lens: Some(body.lens.into()),
        exposure: Exposure {
            time_s: Some(exposure.time.0 as f32 / exposure.time.1 as f32),
            f_number: Some(exposure.f_number as f32 / 100.0),
            iso: Some(u32::from(exposure.iso)),
            bias_ev: Some(exposure.bias as f32 / 3.0),
            focal_mm: Some(exposure.focal as f32 / 1000.0),
            focal_35mm_mm: Some(f32::from(exposure.focal_35)),
        },
        dimensions: Some(Dimensions {
            width: body.size.0,
            height: body.size.1,
        }),
        orientation: ExifOrientation::new(1),
        thumbnail: if thumbnail {
            Some(EmbeddedImage::new(
                0x0800 + inode % 64 * 16,
                5_000 + (inode % 4_000) as u32,
                EmbeddedFormat::Jpeg,
                Some(160),
                Some(107),
            )?)
        } else {
            None
        },
    })
}

/// A time some random hours to days after `from`, and never after "now".
pub fn after(random: &mut Random, from: i64, most_days: i64) -> i64 {
    (from + random.between(2 * HOUR, most_days * 24 * HOUR)).min(now_ms() - HOUR)
}
