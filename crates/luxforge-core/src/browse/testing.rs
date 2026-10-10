//! A generated catalog and index for the browse tests, and a plain model of what they hold, which
//! the tests evaluate every query against independently: straightforward loops over the generated
//! records, sharing nothing with the implementation but the organize functions' signatures.
//!
//! The index holds a card with two bodies of one model, bursts and a RAW+JPEG pair, an indexed
//! folder with nested subfolders, a copy of one file under the same name, lowercase names, undated,
//! pending and unreadable files, an offline archive, and a browsed folder whose name extends the
//! indexed folder's. The catalog holds photographs in nested catalog folders, removed, offline,
//! missing and changed originals, edited ones, a photograph with no capture time and one with no
//! camera, a plain collection with a removed member, a group, and smart collections: a valid one,
//! one over a plain collection, one naming a smart collection and one over files.
use crate::{
    AssetId, EditorService, SourceTag,
    catalog_types::{
        AssetRowId, BodyKey, CameraBody, CaptureTime, CatalogFolder, CatalogFolderId, Collection,
        CollectionId, CollectionKind, EmbeddedFormat, EmbeddedImage, Exposure, FileAvailability,
        FileId, FileRecord, FileSignature, FrameFacts, FrameTables, GeoPosition, GroupLayout,
        HeaderMetadata, HeaderState, IndexRoot, LocalDay, NoProbe, Pick, PlaceNames, RootKind,
        SortKey, Thresholds, ViewFilter, ViewItem, ViewQuery, ViewSource, Volume, VolumeId,
    },
    organize,
    seed::{CatalogSeeder, IndexSeeder, SeedAsset, SeedKind},
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
};

pub(crate) const CATALOG_ID: &str = "catalog-browse-test";
pub(crate) const PICTURES: &str = "/Volumes/SSD/Pictures";
pub(crate) const LAKE: &str = "/Volumes/SSD/Pictures/2026-09-12 Lake";
pub(crate) const SELECTS: &str = "/Volumes/SSD/Pictures/2026-09-12 Lake/selects";
pub(crate) const DUMP: &str = "/Volumes/SSD/Pictures/Card dumps/100LEICA";
pub(crate) const CARD: &str = "/Volumes/NIKON Z 8";
pub(crate) const CARD_1: &str = "/Volumes/NIKON Z 8/DCIM/100NZ8_1";
pub(crate) const CARD_2: &str = "/Volumes/NIKON Z 8/DCIM/101NZ8_2";
pub(crate) const ARCHIVE: &str = "/Volumes/Archive/Trips";
pub(crate) const OSLO: &str = "/Volumes/Archive/Trips/2025 Oslo";
pub(crate) const OLD: &str = "/Volumes/SSD/Pictures-old";
pub(crate) const DEVELOPED: &str = "/Volumes/SSD/Developed";

pub(crate) fn volume(name: &str) -> VolumeId {
    VolumeId::parse(format!("volume-{name}-00000000")).unwrap()
}

pub(crate) fn z8(serial: &str) -> CameraBody {
    CameraBody {
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z 8".into(),
        serial: Some(serial.into()),
    }
}

pub(crate) fn q3() -> CameraBody {
    CameraBody {
        make: "LEICA CAMERA AG".into(),
        model: "LEICA Q3".into(),
        serial: None,
    }
}

fn iphone() -> CameraBody {
    CameraBody {
        make: "Apple".into(),
        model: "iPhone 15 Pro".into(),
        serial: None,
    }
}

fn fuji() -> CameraBody {
    CameraBody {
        make: "FUJIFILM".into(),
        model: "X100VI".into(),
        serial: None,
    }
}

pub(crate) const NIKKOR: &str = "NIKKOR Z 24-120mm f/4 S";
pub(crate) const SUMMILUX: &str = "SUMMILUX 1:1.7/28 ASPH.";
const IPHONE_LENS: &str = "iPhone 15 Pro back camera 6.86mm f/1.78";
const FUJINON: &str = "FUJINON 23mm F2";
const KONSTANZ: (f64, f64) = (47.66, 9.175);
const ZURICH: (f64, f64) = (47.377, 8.54);
const OSLO_AT: (f64, f64) = (59.91, 10.75);

/// A header: `when` is `YYYY:MM:DD HH:MM:SS[.mmm]` and `offset` EXIF's `±HH:MM`.
#[allow(clippy::too_many_arguments)]
fn header(
    when: Option<&str>,
    offset: Option<&str>,
    camera: Option<CameraBody>,
    lens: Option<&str>,
    at: Option<(f64, f64)>,
    iso: u32,
    thumbnail: bool,
) -> HeaderMetadata {
    let capture = when.and_then(|when| {
        let (datetime, subsec) = match when.split_once('.') {
            Some((datetime, subsec)) => (datetime, Some(subsec)),
            None => (when, None),
        };
        CaptureTime::from_exif(datetime, subsec, offset)
    });
    HeaderMetadata {
        capture,
        position: at.and_then(|(lat, lon)| GeoPosition::new(lat, lon, None)),
        camera,
        lens: lens.map(str::to_owned),
        exposure: Exposure {
            time_s: Some(1.0 / 250.0),
            f_number: Some(8.0),
            iso: Some(iso),
            ..Exposure::default()
        },
        dimensions: Some(crate::catalog_types::Dimensions {
            width: 6000,
            height: 4000,
        }),
        orientation: crate::catalog_types::ExifOrientation::new(1),
        thumbnail: thumbnail
            .then(|| EmbeddedImage::new(512, 4096, EmbeddedFormat::Jpeg, None, None).unwrap()),
    }
}

/// One generated file and what the model knows of it.
#[derive(Clone, Debug)]
pub(crate) struct TestFile {
    pub id: FileId,
    pub record: FileRecord,
    pub picked: bool,
    pub developed_as: Option<AssetId>,
    pub offline: bool,
}

impl TestFile {
    pub fn path(&self) -> &str {
        self.record.path.to_str().unwrap()
    }
}

/// One generated photograph and what the model knows of it.
#[derive(Clone, Debug)]
pub(crate) struct TestPhoto {
    pub row: AssetRowId,
    pub seed: SeedAsset,
    /// Its newest edit after its Original, if any.
    pub edited_ms: Option<i64>,
    pub collections: Vec<CollectionId>,
}

/// The generated catalog and index.
pub(crate) struct Fixture {
    pub dir: PathBuf,
    pub catalog: PathBuf,
    pub files: Vec<TestFile>,
    pub photos: Vec<TestPhoto>,
    pub roots: Vec<IndexRoot>,
    pub folders: Vec<CatalogFolder>,
    pub now_ms: i64,
    pub portfolio: CollectionId,
    pub prints: CollectionId,
    pub raw_konstanz: CollectionId,
    pub edited_portfolio: CollectionId,
    pub nested: CollectionId,
    pub of_files: CollectionId,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn file(folder: &str, name: &str, header: HeaderState, volume: &VolumeId) -> FileRecord {
    let kind = if name.to_ascii_lowercase().ends_with(".jpg") {
        SourceTag::Jpeg
    } else {
        SourceTag::Raw
    };
    FileRecord {
        path: Path::new(folder).join(name),
        folder: folder.into(),
        name: name.into(),
        volume_id: volume.clone(),
        signature: FileSignature {
            len: 1000 + name.len() as u64,
            modified_ns: 7,
            identity: None,
        },
        kind,
        header,
        last_seen_ms: 1,
        born_ns: None,
    }
}

fn ok(header: HeaderMetadata) -> HeaderState {
    HeaderState::Ok(Box::new(header))
}

/// Every generated file, with whether it is picked.
fn files() -> Vec<(FileRecord, bool)> {
    let ssd = volume("ssd");
    let card = volume("card");
    let archive = volume("archive");
    let mut files = Vec::new();
    let mut add = |folder: &str, name: &str, header: HeaderState, volume: &VolumeId, picked| {
        files.push((file(folder, name, header, volume), picked));
    };
    let a = z8("3001");
    let z = |when: &str, at, iso, thumbnail| {
        ok(header(
            Some(when),
            Some("+02:00"),
            Some(a.clone()),
            Some(NIKKOR),
            Some(at),
            iso,
            thumbnail,
        ))
    };
    // The card: a burst, singles, a RAW+JPEG pair, a second day, a file with no offset, and an
    // unreadable one.
    add(
        CARD_1,
        "DSC_0001.NEF",
        z("2026:09:12 09:00:00.000", KONSTANZ, 100, true),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0002.NEF",
        z("2026:09:12 09:00:00.300", KONSTANZ, 100, true),
        &card,
        true,
    );
    add(
        CARD_1,
        "DSC_0003.NEF",
        z("2026:09:12 09:00:00.600", KONSTANZ, 100, true),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0004.NEF",
        z("2026:09:12 09:30:00", KONSTANZ, 200, false),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0005.NEF",
        z("2026:09:12 10:15:02.100", KONSTANZ, 200, false),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0006.NEF",
        z("2026:09:12 10:15:02.400", KONSTANZ, 400, false),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0007.NEF",
        z("2026:09:12 11:00:00", KONSTANZ, 100, true),
        &card,
        true,
    );
    add(
        CARD_1,
        "DSC_0007.JPG",
        z("2026:09:12 11:00:00", KONSTANZ, 100, true),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0008.NEF",
        z("2026:09:13 08:00:00", KONSTANZ, 100, false),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0009.NEF",
        z("2026:09:13 08:00:00.500", KONSTANZ, 100, false),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0010.NEF",
        z("2026:09:13 18:00:00", ZURICH, 800, false),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0011.NEF",
        ok(header(
            Some("2026:09:13 23:30:00"),
            None,
            Some(a.clone()),
            Some(NIKKOR),
            None,
            800,
            false,
        )),
        &card,
        false,
    );
    add(
        CARD_1,
        "DSC_0099.NEF",
        HeaderState::Unreadable("truncated TIFF header".into()),
        &card,
        false,
    );
    let b = z8("3002");
    for (name, when) in [
        ("DSC_0101.NEF", "2026:09:13 12:00:00"),
        ("DSC_0102.NEF", "2026:09:13 12:00:01"),
    ] {
        add(
            CARD_2,
            name,
            ok(header(
                Some(when),
                Some("+02:00"),
                Some(b.clone()),
                Some(NIKKOR),
                Some(KONSTANZ),
                100,
                false,
            )),
            &card,
            false,
        );
    }
    let leica = |when: &str| {
        ok(header(
            Some(when),
            Some("+02:00"),
            Some(q3()),
            Some(SUMMILUX),
            Some(KONSTANZ),
            100,
            true,
        ))
    };
    add(
        LAKE,
        "L1000001.DNG",
        leica("2026:09:12 09:05:00"),
        &ssd,
        false,
    );
    add(
        LAKE,
        "L1000002.DNG",
        leica("2026:09:12 09:05:00.800"),
        &ssd,
        false,
    );
    add(
        LAKE,
        "L1000003.JPG",
        leica("2026:09:12 14:00:00"),
        &ssd,
        true,
    );
    add(LAKE, "L1000099.DNG", HeaderState::Pending, &ssd, false);
    // A copy under the same name and time, and a lowercase name.
    add(
        SELECTS,
        "L1000001.DNG",
        leica("2026:09:12 09:05:00"),
        &ssd,
        false,
    );
    add(
        SELECTS,
        "l1000004.dng",
        leica("2026:09:12 15:00:00"),
        &ssd,
        false,
    );
    add(
        DUMP,
        "L1000010.DNG",
        leica("2026:09:13 07:00:00"),
        &ssd,
        false,
    );
    add(
        DUMP,
        "L1000011.DNG",
        leica("2026:09:13 07:00:00.500"),
        &ssd,
        false,
    );
    add(
        PICTURES,
        "IMG_0001.JPG",
        ok(header(
            Some("2026:09:13 13:13:13"),
            Some("+02:00"),
            Some(iphone()),
            Some(IPHONE_LENS),
            Some(KONSTANZ),
            50,
            true,
        )),
        &ssd,
        false,
    );
    add(
        PICTURES,
        "scan-001.jpg",
        ok(header(None, None, None, None, None, 100, false)),
        &ssd,
        false,
    );
    add(
        PICTURES,
        "Scan-002.JPG",
        ok(header(None, None, None, None, None, 100, false)),
        &ssd,
        false,
    );
    // The offline archive.
    let x = |when: Option<&str>| {
        ok(header(
            when,
            Some("+02:00"),
            Some(fuji()),
            Some(FUJINON),
            Some(OSLO_AT),
            160,
            false,
        ))
    };
    add(
        OSLO,
        "DSCF0001.RAF",
        x(Some("2025:06:20 10:00:00")),
        &archive,
        false,
    );
    add(
        OSLO,
        "DSCF0002.RAF",
        x(Some("2025:06:20 10:00:00.200")),
        &archive,
        true,
    );
    add(
        OSLO,
        "DSCF0003.JPG",
        x(Some("2025:06:21 20:00:00")),
        &archive,
        false,
    );
    add(OSLO, "DSCF0004.JPG", x(None), &archive, false);
    // A browsed folder beside the indexed one, in no event.
    for (name, when, picked) in [
        ("OLD_0001.JPG", "2026:08:31 23:30:00", true),
        ("OLD_0002.JPG", "2026:08:31 23:59:59", false),
    ] {
        add(
            OLD,
            name,
            ok(header(
                Some(when),
                None,
                Some(iphone()),
                Some(IPHONE_LENS),
                None,
                50,
                false,
            )),
            &ssd,
            picked,
        );
    }
    files
}

pub(crate) fn roots() -> Vec<IndexRoot> {
    let root = |path: &str, kind, volume, offline| IndexRoot {
        path: path.into(),
        kind,
        volume_id: volume,
        listed_ms: Some(1),
        file_count: None,
        offline,
    };
    vec![
        root(PICTURES, RootKind::Indexed, volume("ssd"), false),
        root(CARD, RootKind::Card, volume("card"), false),
        root(ARCHIVE, RootKind::Indexed, volume("archive"), true),
        root(OLD, RootKind::Browsed, volume("ssd"), false),
    ]
}

fn folder(id: &str, name: &str, parent: Option<&str>) -> CatalogFolder {
    CatalogFolder {
        id: CatalogFolderId::parse(format!("folder-{id}")).unwrap(),
        name: name.into(),
        parent_id: parent.map(|parent| CatalogFolderId::parse(format!("folder-{parent}")).unwrap()),
        created_ms: 1,
        event: None,
        count: 0,
        year: None,
    }
}

pub(crate) fn folder_id(id: &str) -> CatalogFolderId {
    CatalogFolderId::parse(format!("folder-{id}")).unwrap()
}

fn collection_id(name: &str) -> CollectionId {
    CollectionId::parse(format!("collection-{name}")).unwrap()
}

/// One generated photograph's attributes.
struct PhotoSpec {
    folder: &'static str,
    kind: SeedKind,
    locator: String,
    header: HeaderMetadata,
    place: Option<&'static str>,
    developed_days: i64,
    removed: bool,
    availability: FileAvailability,
    edited_days: Option<i64>,
}

fn photos() -> Vec<PhotoSpec> {
    use FileAvailability::{Available, Changed, Missing, Offline};
    let spec =
        |folder, kind, locator: &str, header, place, developed_days, availability| PhotoSpec {
            folder,
            kind,
            locator: locator.into(),
            header,
            place,
            developed_days,
            removed: false,
            availability,
            edited_days: None,
        };
    let at = |when, offset, camera, lens, position| {
        header(Some(when), offset, camera, lens, position, 100, false)
    };
    let k = Some("+02:00");
    let developed = |name: &str| format!("{DEVELOPED}/{name}");
    let mut photos = vec![
        spec(
            "konstanz-2026",
            SeedKind::Raw,
            &format!("{CARD_1}/DSC_0003.NEF"),
            at(
                "2026:09:12 09:00:00.600",
                k,
                Some(z8("3001")),
                Some(NIKKOR),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            3,
            Available,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Raw,
            &format!("{LAKE}/L1000002.DNG"),
            at(
                "2026:09:12 09:05:00.800",
                k,
                Some(q3()),
                Some(SUMMILUX),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            3,
            Available,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Jpeg,
            &developed("K3.JPG"),
            at(
                "2026:09:12 14:00:00",
                k,
                Some(q3()),
                Some(SUMMILUX),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            10,
            Available,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Raw,
            &developed("K4.NEF"),
            at(
                "2026:09:13 08:00:00",
                k,
                Some(z8("3001")),
                Some(NIKKOR),
                Some(ZURICH),
            ),
            Some("Zürich"),
            10,
            Offline,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Raw,
            &developed("K5.NEF"),
            at(
                "2026:09:13 08:00:00",
                k,
                Some(z8("3002")),
                Some(NIKKOR),
                Some(ZURICH),
            ),
            Some("Zürich"),
            45,
            Available,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Jpeg,
            &developed("k6.jpg"),
            header(None, None, None, None, None, 100, false),
            None,
            5,
            Missing,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Raw,
            &format!("{CARD_1}/DSC_0005.NEF"),
            at(
                "2026:09:12 10:15:02.100",
                k,
                Some(z8("3001")),
                Some(NIKKOR),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            20,
            Available,
        ),
        spec(
            "konstanz-selects",
            SeedKind::Raw,
            &developed("selects/S1.DNG"),
            at(
                "2026:09:12 09:05:00",
                k,
                Some(q3()),
                Some(SUMMILUX),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            3,
            Available,
        ),
        spec(
            "konstanz-selects",
            SeedKind::Jpeg,
            &developed("selects/S2.JPG"),
            at(
                "2026:09:13 07:00:00",
                k,
                Some(q3()),
                Some(SUMMILUX),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            60,
            Changed,
        ),
        spec(
            "oslo-2025",
            SeedKind::Raw,
            &format!("{OSLO}/DSCF0001.RAF"),
            at(
                "2025:06:20 10:00:00",
                k,
                Some(fuji()),
                Some(FUJINON),
                Some(OSLO_AT),
            ),
            Some("Oslo"),
            400,
            Offline,
        ),
        spec(
            "oslo-2025",
            SeedKind::Raw,
            &format!("{OSLO}/DSCF0002.RAF"),
            at(
                "2025:06:20 10:00:00.200",
                k,
                Some(fuji()),
                Some(FUJINON),
                Some(OSLO_AT),
            ),
            Some("Oslo"),
            400,
            Offline,
        ),
        spec(
            "oslo-2025",
            SeedKind::Jpeg,
            &format!("{OSLO}/DSCF0003.JPG"),
            at(
                "2025:06:21 20:00:00",
                k,
                Some(fuji()),
                Some(FUJINON),
                Some(OSLO_AT),
            ),
            Some("Oslo"),
            399,
            Available,
        ),
        spec(
            "oslo-2025",
            SeedKind::Jpeg,
            &format!("{OSLO}/DSCF0009.JPG"),
            at(
                "2025:06:21 21:00:00",
                k,
                Some(fuji()),
                Some(FUJINON),
                Some(OSLO_AT),
            ),
            Some("Oslo"),
            399,
            Available,
        ),
        spec(
            "oslo-2025",
            SeedKind::Jpeg,
            &developed("no-camera.jpg"),
            at("2025:06:22 12:00:00", None, None, None, None),
            None,
            398,
            Available,
        ),
        spec(
            "konstanz-2026",
            SeedKind::Jpeg,
            &developed("IMG_0001.JPG"),
            at(
                "2026:09:13 13:13:13",
                k,
                Some(iphone()),
                Some(IPHONE_LENS),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            1,
            Available,
        ),
        spec(
            "konstanz-selects",
            SeedKind::Jpeg,
            &developed("selects/k3.JPG"),
            at(
                "2026:09:12 14:00:00",
                k,
                Some(q3()),
                Some(SUMMILUX),
                Some(KONSTANZ),
            ),
            Some("Konstanz"),
            10,
            Available,
        ),
    ];
    photos[0].edited_days = Some(1);
    photos[2].edited_days = Some(2);
    photos[5].edited_days = Some(4);
    photos[6].removed = true;
    photos[7].edited_days = Some(1);
    photos[10].edited_days = Some(300);
    photos[12].removed = true;
    photos
}

/// Generate the catalog and index in a new scratch directory.
pub(crate) fn fixture(name: &str) -> Fixture {
    let dir = luxforge_testbase::paths::temp_dir(&format!("browse-{name}"));
    let catalog = dir.join("catalog.sqlite");
    let now_ms = crate::editor::now_ms();
    let day = 86_400_000;
    let volumes: Vec<Volume> = [
        ("ssd", "/Volumes/SSD"),
        ("card", CARD),
        ("archive", "/Volumes/Archive"),
    ]
    .into_iter()
    .map(|(name, mount)| Volume {
        id: volume(name),
        mount_point: mount.into(),
        label: name.into(),
        removable: name == "card",
        platform_id: None,
        last_seen_ms: 1,
    })
    .collect();
    let folders = vec![
        folder("konstanz-2026", "Konstanz · Sep 2026", None),
        folder("konstanz-selects", "Selects", Some("konstanz-2026")),
        folder("oslo-2025", "Oslo · 2025", None),
        folder("empty-folder", "Empty", None),
    ];
    let specs = photos();
    let seeds: Vec<SeedAsset> = specs
        .iter()
        .enumerate()
        .map(|(at, spec)| {
            let n = at as u32 + 1;
            let volume = if spec.locator.starts_with("/Volumes/Archive") {
                volume("archive")
            } else if spec.locator.starts_with(CARD) {
                volume("card")
            } else {
                volume("ssd")
            };
            let developed_ms = now_ms - spec.developed_days * day;
            SeedAsset {
                id: AssetId::parse(format!("asset-{n:032x}")).unwrap(),
                kind: spec.kind,
                locator: spec.locator.clone().into(),
                fingerprint: format!("{n:064x}"),
                file_identity: format!("unix:1:{n}"),
                byte_len: 1000,
                width: 6000,
                height: 4000,
                catalog_folder_id: folder_id(spec.folder),
                volume_id: volume,
                developed_ms,
                removed_ms: spec.removed.then_some(now_ms - day),
                availability: spec.availability,
                checked_ms: 1,
                develop_moment: None,
                header: spec.header.clone(),
                place: spec.place.map(str::to_owned),
            }
        })
        .collect();
    let generated = files();
    let mut seeder = CatalogSeeder::create(&catalog, CATALOG_ID).unwrap();
    seeder.volumes(&volumes).unwrap();
    seeder.folders(&folders).unwrap();
    let rows = seeder.assets(&seeds).unwrap();
    let portfolio = collection_id("portfolio");
    let prints = collection_id("prints-group");
    let raw_konstanz = collection_id("raw-konstanz");
    let edited_portfolio = collection_id("edited-portfolio");
    let nested = collection_id("nested-smart");
    let of_files = collection_id("smart-of-files");
    let smart = |id: &CollectionId,
                 name: &str,
                 parent: Option<&CollectionId>,
                 query: serde_json::Value| Collection {
        id: id.clone(),
        name: name.into(),
        parent_id: parent.cloned(),
        kind: CollectionKind::Smart,
        query: Some(serde_json::from_value(query).unwrap()),
        created_ms: 1,
        count: None,
    };
    let collections = vec![
        Collection {
            id: portfolio.clone(),
            name: "Portfolio".into(),
            parent_id: None,
            kind: CollectionKind::Collection,
            query: None,
            created_ms: 1,
            count: None,
        },
        Collection {
            id: prints.clone(),
            name: "Prints".into(),
            parent_id: None,
            kind: CollectionKind::Group,
            query: None,
            created_ms: 1,
            count: None,
        },
        smart(
            &raw_konstanz,
            "Raw Konstanz",
            None,
            json!({
                "source": {"kind": "all-photographs"},
                "filter": {"kinds": ["raw"], "places": ["Konstanz"]},
            }),
        ),
        smart(
            &edited_portfolio,
            "Edited portfolio",
            Some(&prints),
            json!({
                "source": {"kind": "collection", "collection_id": portfolio},
                "filter": {"edited": true},
            }),
        ),
        smart(
            &nested,
            "Nested",
            None,
            json!({
                "source": {"kind": "collection", "collection_id": raw_konstanz},
            }),
        ),
        smart(
            &of_files,
            "Of files",
            None,
            json!({
                "source": {"kind": "folder", "path": PICTURES},
            }),
        ),
    ];
    seeder.collections(&collections).unwrap();
    let members = [0usize, 2, 7, 6, 10];
    seeder
        .members(
            &members
                .iter()
                .map(|at| (portfolio.clone(), rows[*at], 5))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let picks: Vec<Pick> = generated
        .iter()
        .filter(|(_, picked)| *picked)
        .map(|(record, _)| Pick {
            path: record.path.clone(),
            signature: record.signature,
            volume_id: record.volume_id.clone(),
            actor: "test".into(),
            request_id: "pick".into(),
            picked_ms: 1,
            file_id: None,
        })
        .collect();
    seeder.picks(&picks).unwrap();
    seeder.finish().unwrap();
    // Edits beyond the Original: a later entry each, written as the history does.
    let connection = Connection::open(&catalog).unwrap();
    let mut photos = Vec::new();
    for (at, (spec, seed)) in specs.iter().zip(&seeds).enumerate() {
        let edited_ms = spec.edited_days.map(|days| now_ms - days * day + at as i64);
        if let Some(edited_ms) = edited_ms {
            connection
                .execute(
                    "INSERT INTO entries (id, asset_id, sequence, action_id, label, actor,
                         timestamp_ms, undo_parent_id, restore_target_id, entry_json)
                     SELECT ?1, asset_id, 1, 'set-basic', 'Basic', 'test', ?2, NULL, NULL,
                         entry_json
                     FROM entries WHERE asset_id = ?3 AND sequence = 0",
                    params![
                        format!("entry-edit{:028x}", at + 1),
                        edited_ms,
                        seed.id.as_str()
                    ],
                )
                .unwrap();
        }
        photos.push(TestPhoto {
            row: rows[at],
            seed: seed.clone(),
            edited_ms,
            collections: if members.contains(&at) {
                vec![portfolio.clone()]
            } else {
                vec![]
            },
        });
    }
    drop(connection);
    let roots = roots();
    let mut index = IndexSeeder::create(&catalog, CATALOG_ID).unwrap();
    index.roots(&roots).unwrap();
    let records: Vec<FileRecord> = generated.iter().map(|(record, _)| record.clone()).collect();
    let ids = index.files(&records).unwrap();
    index.finish().unwrap();
    let files = generated
        .into_iter()
        .zip(ids)
        .map(|((record, picked), id)| {
            let path = record.path.to_str().unwrap().to_owned();
            let developed_as = photos
                .iter()
                .find(|photo| {
                    photo.seed.removed_ms.is_none() && photo.seed.locator.to_str() == Some(&path)
                })
                .map(|photo| photo.seed.id.clone());
            let offline = record.folder.starts_with(ARCHIVE);
            TestFile {
                id,
                record,
                picked,
                developed_as,
                offline,
            }
        })
        .collect();
    Fixture {
        dir,
        catalog,
        files,
        photos,
        roots,
        folders,
        now_ms,
        portfolio,
        prints,
        raw_konstanz,
        edited_portfolio,
        nested,
        of_files,
    }
}

/// Write the index's revision, as the index lane does with every write batch.
pub(crate) fn set_index_revision(index: &Connection, revision: u64) {
    index
        .execute(
            "INSERT INTO index_meta (key, value) VALUES ('revision', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [revision.to_string()],
        )
        .unwrap();
}

/// Append one library change to the journal, as the library lane will.
pub(crate) fn library_change(service: &EditorService, sequence: u64) {
    service
        .connection
        .execute(
            "INSERT INTO library_changes (sequence, actor, client_key, request_id, method, label,
                 time_ms, item_count)
             VALUES (?1, 'test', 'client', ?2, 'pick.set', 'Picked', 1, 1)",
            params![sequence as i64, format!("request-{sequence}")],
        )
        .unwrap();
}

// ── The model ────────────────────────────────────────────────────────────────────────────────

/// One item as the model sees it.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub item: ViewItem,
    pub folder: String,
    pub name: String,
    pub path: String,
    pub header: HeaderMetadata,
    pub kind: SourceTag,
    pub place: Option<String>,
    pub picked: bool,
    pub in_catalog: bool,
    pub edited: bool,
    pub developed_ms: i64,
    pub last_edited_ms: i64,
    pub availability: FileAvailability,
}

impl Item {
    pub fn instant(&self) -> Option<i64> {
        self.header.capture.as_ref().map(CaptureTime::instant_ms)
    }

    pub fn day(&self) -> Option<LocalDay> {
        self.header.capture.as_ref().map(CaptureTime::local_day)
    }

    pub fn camera_key(&self) -> BodyKey {
        BodyKey::of(self.header.camera.as_ref())
    }
}

impl Fixture {
    pub fn file(&self, path: &str) -> &TestFile {
        self.files
            .iter()
            .find(|file| file.path() == path)
            .unwrap_or_else(|| panic!("no file {path}"))
    }

    pub fn file_items(&self) -> Vec<Item> {
        self.files
            .iter()
            .map(|file| {
                let header = file.record.header.header().cloned().unwrap_or_default();
                // A file's place is the bundled gazetteer's nearest to its position, as a row and
                // a facet read it.
                let place = header
                    .position
                    .as_ref()
                    .and_then(|position| crate::organize::Gazetteer.nearest(position));
                Item {
                    item: ViewItem::File(file.id),
                    folder: file.record.folder.to_str().unwrap().into(),
                    name: file.record.name.clone(),
                    path: file.path().into(),
                    header,
                    kind: file.record.kind,
                    place,
                    picked: file.picked,
                    in_catalog: file.developed_as.is_some(),
                    edited: false,
                    developed_ms: 0,
                    last_edited_ms: 0,
                    availability: if file.offline {
                        FileAvailability::Offline
                    } else {
                        FileAvailability::Available
                    },
                }
            })
            .collect()
    }

    pub fn photo_items(&self) -> Vec<Item> {
        self.photos
            .iter()
            .map(|photo| {
                let seed = &photo.seed;
                Item {
                    item: ViewItem::Photo(photo.row),
                    folder: seed.locator.parent().unwrap().to_str().unwrap().into(),
                    name: seed.locator.file_name().unwrap().to_str().unwrap().into(),
                    path: seed.locator.to_str().unwrap().into(),
                    header: seed.header.clone(),
                    kind: match seed.kind {
                        SeedKind::Jpeg => SourceTag::Jpeg,
                        SeedKind::Raw => SourceTag::Raw,
                    },
                    place: seed.place.clone(),
                    picked: false,
                    in_catalog: true,
                    edited: photo.edited_ms.is_some(),
                    developed_ms: seed.developed_ms,
                    last_edited_ms: photo.edited_ms.unwrap_or(seed.developed_ms),
                    availability: seed.availability,
                }
            })
            .collect()
    }

    /// The files events are computed over: those of the indexed folders and the mounted cards.
    pub fn event_items(&self) -> Vec<Item> {
        let organized: Vec<&IndexRoot> = self
            .roots
            .iter()
            .filter(|root| {
                root.kind == RootKind::Indexed || (root.kind == RootKind::Card && !root.offline)
            })
            .collect();
        self.file_items()
            .into_iter()
            .filter(|item| {
                organized
                    .iter()
                    .any(|root| Path::new(&item.path).starts_with(&root.path))
            })
            .collect()
    }

    /// The events of the model's files, as `organize::events` computes them over its own frames:
    /// each event's identity and files.
    pub fn events(&self) -> Vec<(crate::catalog_types::EventGroup, Vec<FileId>)> {
        let items = self.event_items();
        let (frames, tables) = frames(&items, &items);
        let set = organize::events(
            &frames,
            &tables,
            &Thresholds::default(),
            &crate::organize::Gazetteer,
        );
        set.events
            .iter()
            .map(|group| {
                let mut files: Vec<FileId> = set.order
                    [group.start as usize..(group.start + group.len) as usize]
                    .iter()
                    .map(|at| match frames[*at as usize].item {
                        ViewItem::File(file) => file,
                        ViewItem::Photo(_) => unreachable!(),
                    })
                    .collect();
                files.sort();
                (group.clone(), files)
            })
            .collect()
    }

    /// The items a source holds, before any filter: its definition, restated plainly.
    pub fn source_items(&self, source: &ViewSource) -> Vec<Item> {
        let under = |item: &Item, root: &str| Path::new(&item.path).starts_with(root);
        let live = |photo: &&TestPhoto| photo.seed.removed_ms.is_none();
        let photos = |keep: &dyn Fn(&TestPhoto) -> bool| -> Vec<Item> {
            let rows: Vec<AssetRowId> = self
                .photos
                .iter()
                .filter(|photo| keep(photo))
                .map(|photo| photo.row)
                .collect();
            self.photo_items()
                .into_iter()
                .filter(|item| matches!(item.item, ViewItem::Photo(row) if rows.contains(&row)))
                .collect()
        };
        match source {
            ViewSource::Event { event_id } => {
                let files = self
                    .events()
                    .into_iter()
                    .find(|(group, _)| &group.id == event_id)
                    .map(|(_, files)| files)
                    .unwrap_or_default();
                self.file_items()
                    .into_iter()
                    .filter(
                        |item| matches!(item.item, ViewItem::File(file) if files.contains(&file)),
                    )
                    .collect()
            }
            ViewSource::Folder { path, subfolders } => self
                .file_items()
                .into_iter()
                .filter(|item| {
                    Path::new(&item.folder) == path
                        || (*subfolders && Path::new(&item.folder).starts_with(path))
                })
                .collect(),
            ViewSource::Card { volume_id } => {
                let cards: Vec<&IndexRoot> = self
                    .roots
                    .iter()
                    .filter(|root| root.kind == RootKind::Card && &root.volume_id == volume_id)
                    .collect();
                self.file_items()
                    .into_iter()
                    .filter(|item| {
                        cards
                            .iter()
                            .any(|root| under(item, root.path.to_str().unwrap()))
                    })
                    .collect()
            }
            ViewSource::AllPhotographs => photos(&|photo| live(&photo)),
            ViewSource::RecentlyDeveloped { days } => {
                let since = self.now_ms - i64::from(*days) * 86_400_000;
                photos(&|photo| live(&photo) && photo.seed.developed_ms >= since)
            }
            ViewSource::CatalogFolder {
                folder_id,
                subfolders,
            } => {
                let mut inside = vec![folder_id.clone()];
                if *subfolders {
                    // Every descendant, by repeated passes over the folders.
                    loop {
                        let before = inside.len();
                        for folder in &self.folders {
                            if folder
                                .parent_id
                                .as_ref()
                                .is_some_and(|parent| inside.contains(parent))
                                && !inside.contains(&folder.id)
                            {
                                inside.push(folder.id.clone());
                            }
                        }
                        if inside.len() == before {
                            break;
                        }
                    }
                }
                photos(&|photo| live(&photo) && inside.contains(&photo.seed.catalog_folder_id))
            }
            ViewSource::Collection { collection_id } if collection_id == &self.portfolio => {
                photos(&|photo| live(&photo) && photo.collections.contains(collection_id))
            }
            ViewSource::Collection { collection_id } if collection_id == &self.raw_konstanz => {
                let stored = ViewFilter {
                    kinds: vec![SourceTag::Raw],
                    places: vec!["Konstanz".into()],
                    ..ViewFilter::default()
                };
                self.source_items(&ViewSource::AllPhotographs)
                    .into_iter()
                    .filter(|item| passes(item, &stored, false))
                    .collect()
            }
            ViewSource::Collection { collection_id } if collection_id == &self.edited_portfolio => {
                let stored = ViewFilter {
                    edited: Some(true),
                    ..ViewFilter::default()
                };
                self.source_items(&ViewSource::Collection {
                    collection_id: self.portfolio.clone(),
                })
                .into_iter()
                .filter(|item| passes(item, &stored, false))
                .collect()
            }
            ViewSource::Collection { .. } => panic!("the model answers no other collection"),
            ViewSource::MissingOriginals => photos(&|photo| {
                live(&photo) && photo.seed.availability != FileAvailability::Available
            }),
            ViewSource::Removed => photos(&|photo| photo.seed.removed_ms.is_some()),
        }
    }

    /// The items a source's folders and bodies are recorded from, in row order: a smart
    /// collection's are its stored source's, before its stored filter.
    fn read_items(&self, source: &ViewSource) -> Vec<Item> {
        match source {
            ViewSource::Collection { collection_id } if collection_id == &self.raw_konstanz => {
                self.source_items(&ViewSource::AllPhotographs)
            }
            ViewSource::Collection { collection_id } if collection_id == &self.edited_portfolio => {
                self.source_items(&ViewSource::Collection {
                    collection_id: self.portfolio.clone(),
                })
            }
            source => self.source_items(source),
        }
    }

    /// What `query` should evaluate to.
    pub fn view(&self, query: &ViewQuery) -> Expected {
        let read = self.read_items(&query.source);
        let items = self.source_items(&query.source);
        let decided = decided(&read, &items, &query.thresholds);
        let kept: Vec<Item> = items
            .into_iter()
            .zip(decided)
            .filter(|(item, decided)| passes(item, &query.filter, *decided))
            .map(|(item, _)| item)
            .collect();
        let grouping = query.effective_grouping();
        let (mut frames, tables) = frames(&read, &kept);
        let ordered: Vec<Item> = match query.sort.key {
            SortKey::CaptureTime => {
                organize::order(&mut frames, &tables, grouping, query.sort.descending);
                frames
                    .iter()
                    .map(|frame| {
                        kept.iter()
                            .find(|item| item.item == frame.item)
                            .unwrap()
                            .clone()
                    })
                    .collect()
            }
            _ => {
                let mut sorted = kept.clone();
                sorted.sort_by(|a, b| compare(a, b, query));
                let order: Vec<ViewItem> = sorted.iter().map(|item| item.item).collect();
                frames.sort_by_key(|frame| order.iter().position(|item| *item == frame.item));
                sorted
            }
        };
        let mut layout = organize::group(&frames, &tables, grouping, &query.thresholds, &NoProbe);
        // Each day's and moment's picks, counted over the model's own ordered items.
        let picks = |start: u32, len: u32| {
            ordered[start as usize..(start + len) as usize]
                .iter()
                .filter(|item| item.picked)
                .count() as u32
        };
        for day in &mut layout.days {
            day.picked = picks(day.start, day.len);
        }
        for moment in &mut layout.moments {
            moment.picked = picks(moment.start, moment.len);
        }
        Expected {
            picked: ordered.iter().filter(|item| item.picked).count() as u32,
            in_catalog: ordered.iter().filter(|item| item.in_catalog).count() as u32,
            unavailable: ordered
                .iter()
                .filter(|item| item.availability != FileAvailability::Available)
                .count() as u32,
            items: ordered,
            layout,
        }
    }
}

/// What the model says a query evaluates to.
pub(crate) struct Expected {
    pub items: Vec<Item>,
    pub layout: GroupLayout,
    pub picked: u32,
    pub in_catalog: u32,
    pub unavailable: u32,
}

impl Expected {
    pub fn order(&self) -> Vec<ViewItem> {
        self.items.iter().map(|item| item.item).collect()
    }
}

/// The model's frames for `items`, in their order, with their folders and bodies recorded in the
/// order `read` first names them, as a source's rows are read.
pub(crate) fn frames(read: &[Item], items: &[Item]) -> (Vec<FrameFacts>, FrameTables) {
    let mut tables = FrameTables::default();
    for item in read {
        tables.folder(item.folder.clone().into());
        tables.body(item.header.camera.as_ref());
    }
    let frames = items
        .iter()
        .map(|item| FrameFacts {
            item: item.item,
            folder: tables.folder(item.folder.clone().into()),
            name: item.name.clone().into(),
            instant_ms: item.instant(),
            local_day: item.day(),
            position: item.header.position,
            body: tables.body(item.header.camera.as_ref()),
            exposure: item.header.exposure,
        })
        .collect();
    (frames, tables)
}

/// Which items Moments without a pick leaves out: the source grouped Day › Camera › Moment, and a
/// frame decided when it is picked or in a moment with a picked frame.
fn decided(read: &[Item], items: &[Item], thresholds: &Thresholds) -> Vec<bool> {
    let (mut frames, tables) = frames(read, items);
    organize::order(
        &mut frames,
        &tables,
        crate::catalog_types::Grouping::DayCameraMoment,
        false,
    );
    let layout = organize::group(
        &frames,
        &tables,
        crate::catalog_types::Grouping::DayCameraMoment,
        thresholds,
        &NoProbe,
    );
    let picked = |item: ViewItem| {
        items
            .iter()
            .find(|candidate| candidate.item == item)
            .unwrap()
            .picked
    };
    let mut decided: Vec<(ViewItem, bool)> = frames
        .iter()
        .map(|frame| (frame.item, picked(frame.item)))
        .collect();
    for moment in &layout.moments {
        let range = moment.start as usize..(moment.start + moment.len) as usize;
        if decided[range.clone()].iter().any(|(_, picked)| *picked) {
            for entry in &mut decided[range] {
                entry.1 = true;
            }
        }
    }
    items
        .iter()
        .map(|item| {
            decided
                .iter()
                .find(|(candidate, _)| *candidate == item.item)
                .unwrap()
                .1
        })
        .collect()
}

/// Whether `item` passes every condition of `filter`.
pub(crate) fn passes(item: &Item, filter: &ViewFilter, decided: bool) -> bool {
    let lower = |value: &str| value.to_lowercase();
    if let Some(text) = &filter.text {
        let needle = lower(text);
        let camera = item.header.camera.as_ref().is_some_and(|camera| {
            [camera.label(), camera.make.clone(), camera.model.clone()]
                .iter()
                .any(|value| lower(value).contains(&needle))
        });
        let found = lower(&item.name).contains(&needle)
            || camera
            || item
                .header
                .lens
                .as_deref()
                .is_some_and(|lens| lower(lens).contains(&needle))
            || item
                .place
                .as_deref()
                .is_some_and(|place| lower(place).contains(&needle));
        if !found {
            return false;
        }
    }
    if filter.picked.is_some_and(|picked| picked != item.picked) {
        return false;
    }
    if filter.without_pick && decided {
        return false;
    }
    if !filter.cameras.is_empty() && !filter.cameras.contains(&item.camera_key()) {
        return false;
    }
    if !filter.lenses.is_empty()
        && !item
            .header
            .lens
            .as_ref()
            .is_some_and(|lens| filter.lenses.contains(lens))
    {
        return false;
    }
    if !filter.kinds.is_empty() && !filter.kinds.contains(&item.kind) {
        return false;
    }
    if filter.edited.is_some_and(|edited| edited != item.edited) {
        return false;
    }
    if let Some(dates) = filter.dates
        && !item
            .day()
            .is_some_and(|day| dates.from <= day && day <= dates.to)
    {
        return false;
    }
    if !filter.places.is_empty()
        && !item
            .place
            .as_ref()
            .is_some_and(|place| filter.places.contains(place))
    {
        return false;
    }
    true
}

/// The declared order of a sort other than capture time, restated.
fn compare(a: &Item, b: &Item, query: &ViewQuery) -> Ordering {
    let key = match query.sort.key {
        SortKey::FileName => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::DateDeveloped => a.developed_ms.cmp(&b.developed_ms),
        SortKey::LastEdited => a.last_edited_ms.cmp(&b.last_edited_ms),
        SortKey::CaptureTime => unreachable!(),
    };
    let key = if query.sort.descending {
        key.reverse()
    } else {
        key
    };
    let time = match (a.instant(), b.instant()) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let path = if query.source.over_files() {
        PathBuf::from(&a.folder)
            .cmp(&PathBuf::from(&b.folder))
            .then_with(|| a.name.cmp(&b.name))
    } else {
        Ordering::Equal
    };
    key.then(time)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then(path)
        .then_with(|| a.item.cmp(&b.item))
}
