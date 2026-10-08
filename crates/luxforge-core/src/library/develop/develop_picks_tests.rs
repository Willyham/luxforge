//! The develop lane's parts on their own, over files written here: one read of a JPEG with its
//! EXIF streams the fingerprint and reads its header and interpretation without decoding it; what
//! cannot be developed is refused by its own error; a report is rebuilt from recorded changes; and,
//! on the owner's Mac, how long the RAW fixtures' interpretations take. The helpers write photographs
//! with the EXIF a camera would, for the owner tests (`api/owner/library/develop_picks_tests.rs`).
use super::*;
use crate::{
    AssetId, ErrorKind,
    catalog_types::{
        CameraBody, FileRecord, FileSignature, GeoPosition, HeaderState, LibraryChange,
        LibraryChangeRow, LibraryChangeSeq, VolumeId,
    },
    export::metadata::header::read_header,
    source::JPEG_LIMITS,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// A frame's EXIF as a camera writes it.
#[derive(Clone, Debug)]
pub(crate) struct Shot {
    /// `YYYY:MM:DD HH:MM:SS`.
    pub time: String,
    pub subsec: Option<String>,
    /// `±HH:MM`.
    pub offset: Option<String>,
    /// Degrees north and east.
    pub position: Option<(f64, f64)>,
    pub make: String,
    pub model: String,
    pub serial: Option<String>,
    /// Exposure time and f-number as rationals, and ISO.
    pub exposure: ((u32, u32), (u32, u32), u16),
    pub orientation: Option<u16>,
}

impl Shot {
    /// A frame from a NIKON Z 8 at `time` in Konstanz, 1/250 s at f/8, ISO 100.
    pub(crate) fn at(time: &str) -> Self {
        Self {
            time: time.to_owned(),
            subsec: None,
            offset: Some("+02:00".to_owned()),
            position: Some((47.6603, 9.1758)),
            make: "NIKON CORPORATION".to_owned(),
            model: "NIKON Z 8".to_owned(),
            serial: Some("3001234".to_owned()),
            exposure: ((1, 250), (8, 1), 100),
            orientation: None,
        }
    }

    /// The same frame at another place.
    pub(crate) fn in_place(self, lat: f64, lon: f64) -> Self {
        Self {
            position: Some((lat, lon)),
            ..self
        }
    }

    /// The same frame with its subsecond digits.
    pub(crate) fn subsec(self, digits: &str) -> Self {
        Self {
            subsec: Some(digits.to_owned()),
            ..self
        }
    }
}

/// One IFD entry: tag, type, count and its bytes.
struct Entry(u16, u16, u32, Vec<u8>);

fn ascii(tag: u16, text: &str) -> Entry {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    Entry(tag, 2, bytes.len() as u32, bytes)
}

fn rationals(tag: u16, values: &[(u32, u32)]) -> Entry {
    let bytes = values
        .iter()
        .flat_map(|(num, den)| [num.to_le_bytes(), den.to_le_bytes()].concat())
        .collect();
    Entry(tag, 5, values.len() as u32, bytes)
}

fn short(tag: u16, value: u16) -> Entry {
    Entry(tag, 3, 1, value.to_le_bytes().to_vec())
}

fn long(tag: u16, value: u32) -> Entry {
    Entry(tag, 4, 1, value.to_le_bytes().to_vec())
}

/// A little-endian IFD placed at `at`, its values after its table.
fn ifd(at: u32, entries: &[Entry]) -> Vec<u8> {
    let table = 2 + 12 * entries.len() + 4;
    let mut out = (entries.len() as u16).to_le_bytes().to_vec();
    let mut data = Vec::new();
    for Entry(tag, kind, count, bytes) in entries {
        out.extend(tag.to_le_bytes());
        out.extend(kind.to_le_bytes());
        out.extend(count.to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes.clone();
            inline.resize(4, 0);
            out.extend(inline);
        } else {
            out.extend((at + (table + data.len()) as u32).to_le_bytes());
            data.extend(bytes);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    out.extend(0_u32.to_le_bytes());
    out.extend(data);
    out
}

/// A degree value as three rationals: degrees, minutes, seconds to the thousandth.
fn dms(value: f64) -> [(u32, u32); 3] {
    let value = value.abs();
    let degrees = value.trunc();
    let minutes = ((value - degrees) * 60.0).trunc();
    let seconds = ((value - degrees) * 60.0 - minutes) * 60.0;
    [
        (degrees as u32, 1),
        (minutes as u32, 1),
        ((seconds * 1000.0).round() as u32, 1000),
    ]
}

/// The APP1 payload of `shot`: `Exif\0\0` and a TIFF with IFD0, the Exif IFD and the GPS IFD.
fn exif(shot: &Shot) -> Vec<u8> {
    let ((time_num, time_den), f_number, iso) = shot.exposure;
    let mut exif_entries = vec![
        rationals(0x829a, &[(time_num, time_den)]),
        rationals(0x829d, &[f_number]),
        short(0x8827, iso),
        ascii(0x9003, &shot.time),
    ];
    if let Some(offset) = &shot.offset {
        exif_entries.push(ascii(0x9011, offset));
    }
    if let Some(subsec) = &shot.subsec {
        exif_entries.push(ascii(0x9291, subsec));
    }
    if let Some(serial) = &shot.serial {
        exif_entries.push(ascii(0xa431, serial));
    }
    let gps_entries = shot.position.map(|(lat, lon)| {
        vec![
            ascii(0x0001, if lat < 0.0 { "S" } else { "N" }),
            rationals(0x0002, &dms(lat)),
            ascii(0x0003, if lon < 0.0 { "W" } else { "E" }),
            rationals(0x0004, &dms(lon)),
        ]
    });
    let ifd0 = |exif_at: u32, gps_at: u32| {
        let mut entries = vec![ascii(0x010f, &shot.make), ascii(0x0110, &shot.model)];
        if let Some(orientation) = shot.orientation {
            entries.push(short(0x0112, orientation));
        }
        entries.push(long(0x8769, exif_at));
        if gps_entries.is_some() {
            entries.push(long(0x8825, gps_at));
        }
        ifd(8, &entries)
    };
    let exif_at = 8 + ifd0(0, 0).len() as u32;
    let exif_ifd = ifd(exif_at, &exif_entries);
    let gps_at = exif_at + exif_ifd.len() as u32;
    let mut tiff = b"II\x2a\x00".to_vec();
    tiff.extend(8_u32.to_le_bytes());
    tiff.extend(ifd0(exif_at, gps_at));
    tiff.extend(exif_ifd);
    if let Some(entries) = &gps_entries {
        tiff.extend(ifd(gps_at, entries));
    }
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(tiff);
    payload
}

/// Write the synthetic 480 × 320 JPEG with `shot`'s EXIF in place of its own at `path`, making its
/// folder, and answer its canonical path. Frames of different shots have different bytes.
pub(crate) fn photo(path: &Path, shot: &Shot) -> PathBuf {
    let fixture = fs::read(luxforge_testbase::paths::jpeg()).unwrap();
    let mut bytes = fixture[..2].to_vec();
    let mut at = 2;
    // Keep every segment before the scan but the fixture's own Exif APP1, and put this one first.
    let payload = exif(shot);
    bytes.extend([0xff, 0xe1]);
    bytes.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    bytes.extend(&payload);
    while fixture[at] == 0xff && fixture[at + 1] != 0xda {
        let len = u16::from_be_bytes([fixture[at + 2], fixture[at + 3]]) as usize;
        let segment = &fixture[at..at + 2 + len];
        if !(fixture[at + 1] == 0xe1 && segment[4..].starts_with(b"Exif\0\0")) {
            bytes.extend(segment);
        }
        at += 2 + len;
    }
    bytes.extend(&fixture[at..]);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    path.canonicalize().unwrap()
}

/// A file the RAW decoder refuses: a TIFF header and nothing a camera writes after it.
pub(crate) fn broken_raw(path: &Path) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
    bytes.resize(4096, 0);
    fs::write(path, bytes).unwrap();
    path.canonicalize().unwrap()
}

/// The index's record of the file at `path` on `volume`, its header read as the index reads it.
pub(crate) fn record(path: &Path, volume: &VolumeId) -> FileRecord {
    let bytes = fs::read(path).unwrap();
    let header = read_header(&mut Cursor::new(&bytes[..]), bytes.len() as u64)
        .unwrap()
        .header
        .metadata();
    FileRecord {
        path: path.to_path_buf(),
        folder: path.parent().unwrap().to_path_buf(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        volume_id: volume.clone(),
        signature: FileSignature::of(&path.metadata().unwrap()),
        kind: if bytes.starts_with(&[0xff, 0xd8]) {
            crate::SourceTag::Jpeg
        } else {
            crate::SourceTag::Raw
        },
        header: HeaderState::Ok(Box::new(header)),
        last_seen_ms: 1,
        born_ns: None,
    }
}

/// What a test checks an original is unchanged by: its bytes' hash, its length, its modification
/// time and its path.
pub(crate) fn untouched(path: &Path) -> (String, u64, std::time::SystemTime, PathBuf) {
    let metadata = path.metadata().unwrap();
    (
        format!("{:x}", Sha256::digest(fs::read(path).unwrap())),
        metadata.len(),
        metadata.modified().unwrap(),
        path.canonicalize().unwrap(),
    )
}

/// One read of a JPEG with a camera's EXIF: the fingerprint is the SHA-256 of its bytes, streamed a
/// chunk at a time; its header is the index reader's; its interpretation is its size turned upright
/// by its orientation, read without decoding it; the gazetteer names its place; and the file is as
/// it was. The read keeps nothing unless asked, and a read asked to keep what it read, as a
/// one-file Develop's is, keeps exactly the bytes it hashed.
#[test]
fn develop_picks_read_streams_the_fingerprint_and_interprets_a_jpeg_from_its_header() {
    let dir = luxforge_testbase::paths::temp_dir("develop-read");
    let shot = Shot {
        orientation: Some(6),
        ..Shot::at("2026:09:12 10:15:02").subsec("13")
    };
    let path = photo(&dir.join("DSC_0412.JPG"), &shot);
    let before = untouched(&path);
    let phases = Mutex::new(Vec::new());
    let read = read(&path, false, &JobControl::new(), &|phase| {
        phases.lock().unwrap().push(phase)
    })
    .unwrap();
    assert!(read.kept.is_none(), "{:?}", read.kept);
    let kept = super::read(&path, true, &JobControl::new(), &|_| {}).unwrap();
    let Some(crate::editor::ReadContent::Jpeg(bytes)) = &kept.kept else {
        panic!("a JPEG's bytes are kept: {:?}", kept.kept);
    };
    assert_eq!(**bytes, fs::read(&path).unwrap());
    assert_eq!(kept.fingerprint, before.0);
    assert_eq!(read.path, path);
    assert_eq!(read.fingerprint, before.0);
    assert_eq!(read.signature.byte_len(), before.1);
    assert_eq!(read.source, crate::SourceKind::Jpeg);
    assert_eq!((read.width, read.height), (320, 480), "turned upright");
    let header = &read.header;
    assert_eq!(
        header.capture.as_ref().unwrap().text,
        "2026-09-12T10:15:02.13+02:00"
    );
    assert_eq!(
        header.camera,
        Some(CameraBody {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z 8".into(),
            serial: Some("3001234".into()),
        })
    );
    let position: GeoPosition = header.position.unwrap();
    assert!((position.lat - 47.6603).abs() < 1e-4 && (position.lon - 9.1758).abs() < 1e-4);
    assert_eq!(header.exposure.iso, Some(100));
    assert_eq!(
        header.dimensions.map(|size| (size.width, size.height)),
        Some((480, 320)),
        "the stored size"
    );
    assert_eq!(
        header.orientation.map(|orientation| orientation.get()),
        Some(6)
    );
    assert_eq!(read.place.as_deref(), Some("Konstanz"));
    assert_eq!(
        *phases.lock().unwrap(),
        [Phase::Hashing, Phase::Hashing, Phase::Hashed],
        "one chunk, then the end"
    );
    // The JPEG's header was checked as a decode checks it, and the file was only read.
    luxforge_jpeg::Decoder::new(&fs::read(&path).unwrap(), JPEG_LIMITS).unwrap();
    assert_eq!(untouched(&path), before);
    fs::remove_dir_all(dir).unwrap();
}

/// A file the RAW decoder refuses, a JPEG in a colour space Luxforge does not open, a missing file
/// and a cancel are each refused as themselves; a file that changes while it is read is `conflict`.
#[test]
fn develop_picks_read_refuses_what_cannot_be_developed() {
    let dir = luxforge_testbase::paths::temp_dir("develop-refuse");
    let broken = broken_raw(&dir.join("DSC_0001.NEF"));
    let error = read(&broken, false, &JobControl::new(), &|_| {}).unwrap_err();
    assert!(
        matches!(error.kind, ErrorKind::Decode | ErrorKind::UnsupportedInput),
        "{error:?}"
    );
    let cmyk = dir.join("cmyk.jpg");
    fs::copy(luxforge_testbase::paths::fixture("s0/cmyk.jpg"), &cmyk).unwrap();
    let error = read(&cmyk, false, &JobControl::new(), &|_| {}).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedColor, "{error:?}");
    let error = read(&dir.join("gone.jpg"), false, &JobControl::new(), &|_| {}).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable, "{error:?}");

    let path = photo(&dir.join("a.jpg"), &Shot::at("2026:09:12 10:00:00"));
    let control = JobControl::new();
    let error = read(&path, false, &control, &|phase| {
        if phase == Phase::Hashing {
            control.cancel("stopped");
        }
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    let error = read(&path, false, &JobControl::new(), &|phase| {
        if phase == Phase::Hashed {
            let file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            let metadata = file.metadata().unwrap();
            let len = metadata.len();
            file.set_len(len - 1).unwrap();
            file.set_len(len).unwrap();
            // Shrinking and regrowing within one filesystem clock tick need not change the
            // signature. Explicitly change it so this fixture proves the post-read conflict.
            file.set_modified(metadata.modified().unwrap() + std::time::Duration::from_secs(1))
                .unwrap();
        }
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict, "{error:?}");
    fs::remove_dir_all(dir).unwrap();
}

/// A report rebuilt from recorded changes pairs each photograph a change created or relinked with
/// the pick it cleared after it — a copy developed in its pick's place is its `used` — and leaves
/// out a pick linked to a photograph already in the catalog.
#[test]
fn develop_picks_recorded_report_pairs_each_photograph_with_its_pick() {
    let (created, copied, relinked) = (AssetId::new(), AssetId::new(), AssetId::new());
    let pick = |path: &str| LibraryChangeRow {
        item: LibraryItem::Pick { path: path.into() },
        before: Some(json!({"path": path})),
        after: None,
    };
    let part = |sequence: u64, rows: Vec<LibraryChangeRow>| LibraryChangeDetail {
        change: LibraryChange {
            sequence: LibraryChangeSeq(sequence),
            actor: "a".into(),
            request_id: "r".into(),
            method: "pick.develop".into(),
            label: "Developed".into(),
            time_ms: 1,
            item_count: rows.len() as u32,
            undoes: None,
            redoes: None,
            undone_by: None,
        },
        rows,
    };
    let developed = |asset: &AssetId, path: &str| LibraryChangeRow {
        item: LibraryItem::DevelopedAsset {
            asset_id: asset.clone(),
        },
        before: None,
        after: Some(json!({"path": path})),
    };
    let report = recorded_report(&[
        part(4, vec![developed(&created, "/a/1.jpg"), pick("/a/1.jpg")]),
        part(
            5,
            vec![
                developed(&copied, "/copy/2.jpg"),
                pick("/card/2.jpg"),
                pick("/a/linked.jpg"),
                LibraryChangeRow {
                    item: LibraryItem::AssetSource {
                        asset_id: relinked.clone(),
                    },
                    before: Some(json!({"locator": "/old/3.jpg"})),
                    after: Some(json!({"locator": "/a/3.jpg"})),
                },
                pick("/a/3.jpg"),
            ],
        ),
    ]);
    assert_eq!(report.changes, [LibraryChangeSeq(4), LibraryChangeSeq(5)]);
    assert_eq!(
        serde_json::to_value(&report.developed).unwrap(),
        json!([
            {"path": "/a/1.jpg", "asset_id": created, "outcome": "created"},
            {"path": "/card/2.jpg", "used": "/copy/2.jpg", "asset_id": copied, "outcome": "created"},
            {"path": "/a/3.jpg", "asset_id": relinked, "outcome": "relinked"},
        ])
    );
    assert!(report.failed.is_empty());
}

/// How long each of the owner's RAW fixtures takes to read and interpret without developing:
/// the streamed read and fingerprint, the header, and `RawSource::decode`, which unpacks the mosaic
/// to answer the interpretation. Set `LUXFORGE_RAW_OWNER_DIR` to the directory holding
/// `nikon_z6.NEF`, `fujifilm_x100vi.RAF` and `mavic_air_2s.DNG`, and run it with `--release
/// --ignored --nocapture`.
#[test]
#[ignore = "requires the owner's RAW fixtures: set LUXFORGE_RAW_OWNER_DIR"]
fn develop_picks_reads_the_owners_raw_interpretations_without_developing() {
    let owner = PathBuf::from(std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("RAW directory"));
    for name in ["nikon_z6.NEF", "fujifilm_x100vi.RAF", "mavic_air_2s.DNG"] {
        let path = owner.join(name);
        let before = untouched(&path);
        let mut times = Vec::new();
        for _ in 0..3 {
            let control = JobControl::new();
            let start = std::time::Instant::now();
            let (_, _, bytes, fingerprint) =
                super::read::read_bytes(&path, &control, &|_| {}).unwrap();
            let read_ms = start.elapsed().as_secs_f64() * 1e3;
            let decode = std::time::Instant::now();
            let sensor = luxforge_raw::RawSource::decode(bytes, control.flag()).unwrap();
            let decode_ms = decode.elapsed().as_secs_f64() * 1e3;
            drop(sensor);
            assert_eq!(fingerprint, before.0);
            let start = std::time::Instant::now();
            let whole = read(&path, false, &control, &|_| {}).unwrap();
            let whole_ms = start.elapsed().as_secs_f64() * 1e3;
            assert!(matches!(whole.source, crate::SourceKind::Raw { .. }));
            times.push((read_ms, decode_ms, whole_ms));
        }
        println!(
            "{name}: {} bytes; read+hash, decode (unpack, no demosaic), whole read in ms: {times:.1?}",
            before.1
        );
        assert_eq!(untouched(&path), before);
    }
}
