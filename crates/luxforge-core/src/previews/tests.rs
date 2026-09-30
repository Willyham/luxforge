//! The preview cache and lane without the owner: keys and staleness by signature, the grid's two
//! stages, upright tiers, camera JPEGs with bytes after EOI, the loupe budget and its eviction,
//! a changed or missing file, the queue's order and bound, the failures remembered, and the reads
//! other lanes use. Ignored tests run the generated image folders and the owner's authentic RAW
//! files.
use super::{
    FILE_GRID_SIDE,
    cache::{self, Store},
    extract,
    lane::{self, Failures, Outcome, PREVIEW_QUEUE_CAPACITY, Pushed, Queue, Task, TaskKey},
};
use crate::{
    Error, ErrorKind, SourceTag,
    catalog_types::{
        EmbeddedFormat, EmbeddedImage, ExifOrientation, FileId, FileRecord, FileSignature,
        HeaderMetadata, HeaderState, LOUPE_MAX_SIDE, PreviewOrigin, PreviewPriority, PreviewState,
        PreviewTier, SHARED_PREVIEW_BUDGET_BYTES, ViewItem, VolumeId,
    },
    index::{IndexDb, upsert_file},
    jobs::JobControl,
};
use exif::{Field, In, Tag, Value as ExifValue, experimental::Writer};
use luxforge_testbase::paths::{self, temp_dir};
use std::{
    cell::Cell,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

/// A scratch index and the files it lists.
pub(crate) struct Fixture {
    pub root: PathBuf,
    pub index: IndexDb,
}

impl Fixture {
    pub(crate) fn new(name: &str) -> Self {
        let root = temp_dir(name);
        let (index, _) = IndexDb::open(&root.join("catalog.index"), "catalog-test").unwrap();
        Self { root, index }
    }

    fn store(&self) -> Store {
        Store::new(self.index.connect().unwrap(), self.index.previews_dir())
    }

    /// Write `bytes` as `name` in the scratch folder.
    pub(crate) fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    /// List `path` in the index as the index lane would, with `header`.
    pub(crate) fn add(&mut self, path: &Path, kind: SourceTag, header: HeaderState) -> FileId {
        add_file(&mut self.index, path, kind, header)
    }

    fn tiers(&self, file: FileId) -> cache::FileTiers {
        cache::file_tiers(self.index.connection(), file)
            .unwrap()
            .expect("the file is listed")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// List `path` in `index` as the index lane would, with its signature now and `header`.
pub(crate) fn add_file(
    index: &mut IndexDb,
    path: &Path,
    kind: SourceTag,
    header: HeaderState,
) -> FileId {
    let metadata = fs::metadata(path).unwrap();
    let record = FileRecord {
        path: path.to_path_buf(),
        folder: path.parent().unwrap().to_path_buf(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        volume_id: VolumeId::parse("volume-0123456789").unwrap(),
        signature: FileSignature::of(&metadata),
        kind,
        header,
        last_seen_ms: 0,
    };
    let tx = index.connection_mut().transaction().unwrap();
    let id = upsert_file(&tx, &record).unwrap();
    tx.commit().unwrap();
    id
}

/// A header that records `orientation` and, when given, where a JPEG thumbnail of `size` is.
pub(crate) fn header(orientation: u8, thumbnail: Option<(u64, u32, (u32, u32))>) -> HeaderState {
    HeaderState::Ok(Box::new(HeaderMetadata {
        orientation: ExifOrientation::new(orientation),
        thumbnail: thumbnail.map(|(offset, len, (width, height))| {
            EmbeddedImage::new(offset, len, EmbeddedFormat::Jpeg, Some(width), Some(height))
                .unwrap()
        }),
        ..HeaderMetadata::default()
    }))
}

/// Red, green, blue and yellow quadrants, as RGBA: top left, top right, bottom left, bottom right.
const QUADRANTS: [[u8; 3]; 4] = [[220, 35, 45], [35, 190, 65], [40, 70, 220], [235, 195, 30]];

fn quadrants(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let quadrant = usize::from(y >= height / 2) * 2 + usize::from(x >= width / 2);
            rgba.extend_from_slice(&QUADRANTS[quadrant]);
            rgba.push(255);
        }
    }
    rgba
}

fn encode(width: u32, height: u32, rgba: &[u8], segments: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    luxforge_jpeg::encode::<_, Error>(
        &mut out,
        width,
        height,
        rgba,
        &luxforge_jpeg::Settings {
            quality: 95,
            chroma: (1, 1),
            segments,
            icc: None,
        },
        &mut |_| Ok(()),
    )
    .unwrap();
    out
}

/// A camera JPEG of `size` quadrants whose EXIF APP1 records `orientation` and holds a `thumb`
/// sized thumbnail of the same picture in IFD1: its bytes, and where the thumbnail is.
pub(crate) fn camera_jpeg(
    size: (u32, u32),
    thumb: (u32, u32),
    orientation: u16,
) -> (Vec<u8>, u64, u32) {
    let thumbnail = encode(thumb.0, thumb.1, &quadrants(thumb.0, thumb.1), &[]);
    let fields = [
        Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: ExifValue::Short(vec![orientation]),
        },
        Field {
            tag: Tag::Orientation,
            ifd_num: In::THUMBNAIL,
            value: ExifValue::Short(vec![1]),
        },
    ];
    let mut writer = Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    writer.set_jpeg(&thumbnail, In::THUMBNAIL);
    let mut tiff = Cursor::new(Vec::new());
    writer.write(&mut tiff, false).unwrap();
    let payload = [b"Exif\0\0".as_slice(), &tiff.into_inner()].concat();
    let bytes = encode(size.0, size.1, &quadrants(size.0, size.1), &[(1, &payload)]);
    let offset = bytes
        .windows(thumbnail.len())
        .position(|window| window == thumbnail.as_slice())
        .expect("the thumbnail is in the file");
    (bytes, offset as u64, thumbnail.len() as u32)
}

fn task((file, tier): (FileId, PreviewTier)) -> Task {
    let key: TaskKey = (ViewItem::File(file), tier);
    Task {
        key,
        control: JobControl::new(),
        budget: SHARED_PREVIEW_BUDGET_BYTES,
        develops: true,
        camera: None,
        develop: None,
        hold: None,
        stage_hold: None,
    }
}

/// Run one task on `store`, answering its outcome and how many thumbnail stages it wrote.
fn run(store: &mut Store, task: &Task) -> (Outcome, usize) {
    let stages = Cell::new(0);
    let outcome = lane::run(store, task, &|| stages.set(stages.get() + 1));
    (outcome, stages.get())
}

/// A preview's pixels, decoded by the independent decoder.
pub(crate) fn decoded(path: &Path) -> image::RgbaImage {
    image::load_from_memory_with_format(&fs::read(path).unwrap(), image::ImageFormat::Jpeg)
        .unwrap()
        .to_rgba8()
}

/// The colour at the centre of each quadrant of an upright picture, in [`QUADRANTS`]' order.
fn quadrant_colours(image: &image::RgbaImage) -> [[u8; 3]; 4] {
    let (width, height) = image.dimensions();
    [(1, 1), (3, 1), (1, 3), (3, 3)].map(|(x, y)| {
        let pixel = image.get_pixel(width * x / 4, height * y / 4);
        [pixel[0], pixel[1], pixel[2]]
    })
}

fn close(actual: [u8; 3], expected: [u8; 3], tolerance: u8) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.abs_diff(expected) <= tolerance)
}

#[test]
fn preview_cache_keys_and_names_follow_what_made_them() {
    let signature = FileSignature {
        len: 10,
        modified_ns: 5,
        identity: None,
    };
    let changed = FileSignature {
        modified_ns: 6,
        ..signature
    };
    let root = Path::new("/c.index/previews");
    let path = |signature: &FileSignature, tier, origin| {
        cache::preview_path(root, FileId(300), signature, tier, origin)
    };
    let grid = path(&signature, PreviewTier::Grid, PreviewOrigin::Embedded);
    assert_eq!(
        grid,
        path(&signature, PreviewTier::Grid, PreviewOrigin::Embedded)
    );
    assert!(grid.starts_with("/c.index/previews/files/2c"), "{grid:?}");
    for other in [
        path(&changed, PreviewTier::Grid, PreviewOrigin::Embedded),
        path(&signature, PreviewTier::Loupe, PreviewOrigin::Embedded),
        path(&signature, PreviewTier::Grid, PreviewOrigin::ExifThumbnail),
    ] {
        assert_ne!(grid, other, "a replacement is a new path");
    }
    let key = cache::key(
        FileId(300),
        &signature,
        PreviewTier::Grid,
        PreviewOrigin::Embedded,
    );
    assert!(key.starts_with("file:300:"), "{key}");
    assert!(key.ends_with(":grid:embedded"), "{key}");
    assert_ne!(
        key,
        cache::key(
            FileId(300),
            &changed,
            PreviewTier::Grid,
            PreviewOrigin::Embedded
        )
    );
}

/// A grid tier is written in two stages: the file's EXIF thumbnail, which the grid can draw at
/// once, then its embedded preview, which replaces it; the thumbnail stage's file goes after the
/// row changed.
#[test]
fn preview_cache_a_grid_arrives_as_its_thumbnail_then_its_embedded_preview() {
    let mut fixture = Fixture::new("preview-cache-stages");
    let (bytes, offset, len) = camera_jpeg((640, 427), (160, 107), 1);
    let path = fixture.file("DSC_0001.JPG", &bytes);
    let file = fixture.add(
        &path,
        SourceTag::Jpeg,
        header(1, Some((offset, len, (160, 107)))),
    );
    let mut store = fixture.store();
    let seen = std::cell::RefCell::new(None);
    let outcome = lane::run(&mut store, &task((file, PreviewTier::Grid)), &|| {
        *seen.borrow_mut() = fixture.tiers(file).grid;
    });
    let thumbnail = seen
        .into_inner()
        .expect("the thumbnail stage was written first");
    assert_eq!(thumbnail.origin, PreviewOrigin::ExifThumbnail);
    assert_eq!((thumbnail.width, thumbnail.height), (160, 107));
    assert!(!thumbnail.complete());
    let preview = outcome.result.unwrap();
    assert_eq!(preview.origin, PreviewOrigin::Embedded);
    assert_eq!((preview.width, preview.height), (512, 342));
    assert_eq!(decoded(&preview.path).dimensions(), (512, 342));
    assert!(!thumbnail.path.exists(), "the replaced stage is removed");
    let tiers = fixture.tiers(file);
    assert_eq!(tiers.grid.unwrap().info(), preview);
    assert!(preview.key.ends_with(":grid:embedded"));
    assert!(preview.path.starts_with(fixture.index.previews_dir()));
    assert_eq!(fs::read(&path).unwrap(), bytes, "the original is untouched");

    // A loupe tier of a 640 px file is the file at its own size: never enlarged.
    let (outcome, stages) = run(&mut store, &task((file, PreviewTier::Loupe)));
    let loupe = outcome.result.unwrap();
    assert_eq!(stages, 0, "a loupe has no thumbnail stage");
    assert_eq!((loupe.width, loupe.height), (640, 427));
    assert_eq!(loupe.origin, PreviewOrigin::Embedded);
}

/// A file with no recorded thumbnail goes straight to its embedded preview.
#[test]
fn preview_cache_a_file_without_a_thumbnail_has_one_stage() {
    let mut fixture = Fixture::new("preview-cache-one-stage");
    let (bytes, ..) = camera_jpeg((900, 600), (160, 107), 1);
    let path = fixture.file("IMG_0001.JPG", &bytes);
    let file = fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let (outcome, stages) = run(&mut fixture.store(), &task((file, PreviewTier::Grid)));
    assert_eq!(stages, 0);
    let preview = outcome.result.unwrap();
    assert_eq!((preview.width, preview.height), (512, 341));
    assert_eq!(preview.origin, PreviewOrigin::Embedded);
}

/// A row made from another signature is never served and is removed, with its file, by the next
/// task that makes the tier; so is a row whose file is missing or empty.
#[test]
fn preview_cache_a_stale_or_broken_row_is_never_served_and_removed_when_found() {
    let mut fixture = Fixture::new("preview-cache-stale");
    let (bytes, ..) = camera_jpeg((640, 427), (160, 107), 1);
    let path = fixture.file("A.JPG", &bytes);
    let file = fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let mut store = fixture.store();
    let first = run(&mut store, &task((file, PreviewTier::Grid)))
        .0
        .result
        .unwrap();
    assert!(fixture.tiers(file).grid.is_some());

    // The file is rewritten: the index row's signature moves on, and the old row is stale.
    let (other, ..) = camera_jpeg((700, 427), (160, 97), 1);
    fs::write(&path, &other).unwrap();
    fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    assert_eq!(fixture.tiers(file).grid, None, "never served");
    assert_eq!(
        cache::grid_states(fixture.index.connection(), &[file]).unwrap(),
        [PreviewState::Pending]
    );
    let second = run(&mut store, &task((file, PreviewTier::Grid)))
        .0
        .result
        .unwrap();
    assert!(!first.path.exists(), "the stale file is removed");
    assert_ne!(first.key, second.key);
    assert_eq!((second.width, second.height), (512, 312));

    // An emptied cache file is a miss: the row goes, and the tier is made again.
    fs::write(&second.path, b"").unwrap();
    let signature = fixture.tiers(file).signature;
    assert_eq!(
        store.current(file, PreviewTier::Grid, &signature).unwrap(),
        None
    );
    assert!(!second.path.exists());
    assert_eq!(fixture.tiers(file).grid, None);
}

/// Every tier is upright for EXIF orientations 1 to 8: the S0 fixtures' quadrants land where
/// the editor's own decode puts them.
#[test]
fn preview_cache_tiers_are_upright_for_every_orientation() {
    let permutations = [
        [0, 1, 2, 3],
        [1, 0, 3, 2],
        [3, 2, 1, 0],
        [2, 3, 0, 1],
        [0, 2, 1, 3],
        [2, 0, 3, 1],
        [3, 1, 2, 0],
        [1, 3, 0, 2],
    ];
    let mut fixture = Fixture::new("preview-cache-orientation");
    let mut store = fixture.store();
    for orientation in 1..=8u8 {
        let source = paths::fixture(&format!("s0/orientation-{orientation}.jpg"));
        let bytes = fs::read(&source).unwrap();
        let path = fixture.file(&format!("orientation-{orientation}.jpg"), &bytes);
        let file = fixture.add(&path, SourceTag::Jpeg, header(orientation, None));
        for tier in [PreviewTier::Grid, PreviewTier::Loupe] {
            let preview = run(&mut store, &task((file, tier))).0.result.unwrap();
            let expected = if orientation >= 5 {
                (320, 480)
            } else {
                (480, 320)
            };
            assert_eq!((preview.width, preview.height), expected, "{orientation}");
            let colours = quadrant_colours(&decoded(&preview.path));
            for (index, colour) in colours.into_iter().enumerate() {
                let want = [[220, 35, 45], [35, 190, 65], [40, 70, 220], [235, 195, 30]]
                    [permutations[usize::from(orientation) - 1][index]];
                assert!(
                    close(colour, want, 12),
                    "orientation {orientation}, {tier:?}, quadrant {index}: {colour:?} against {want:?}"
                );
            }
        }
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

/// The thumbnail stage is turned upright by the file's orientation too: it shows what the
/// embedded stage shows.
#[test]
fn preview_cache_the_thumbnail_stage_is_upright() {
    let mut fixture = Fixture::new("preview-cache-thumbnail-upright");
    for orientation in [3u8, 6, 8] {
        let (bytes, offset, len) = camera_jpeg((640, 400), (160, 100), u16::from(orientation));
        let path = fixture.file(&format!("o{orientation}.JPG"), &bytes);
        let file = fixture.add(
            &path,
            SourceTag::Jpeg,
            header(orientation, Some((offset, len, (160, 100)))),
        );
        let mut store = fixture.store();
        let thumbnail = std::cell::RefCell::new(None);
        let outcome = lane::run(&mut store, &task((file, PreviewTier::Grid)), &|| {
            let stage = fixture.tiers(file).grid.unwrap();
            *thumbnail.borrow_mut() = Some(decoded(&stage.path));
        });
        let thumbnail = thumbnail.into_inner().unwrap();
        let embedded = decoded(&outcome.result.unwrap().path);
        let expected = if orientation >= 5 {
            (100, 160)
        } else {
            (160, 100)
        };
        assert_eq!(thumbnail.dimensions(), expected);
        for (stage, preview) in quadrant_colours(&thumbnail)
            .into_iter()
            .zip(quadrant_colours(&embedded))
        {
            assert!(
                close(stage, preview, 16),
                "{orientation}: {stage:?} {preview:?}"
            );
        }
    }
}

/// A camera JPEG with bytes after its EOI marker decodes: it is cut at its last EOI first, and
/// the strict codec stays strict.
#[test]
fn preview_cache_a_jpeg_with_bytes_after_eoi_decodes() {
    let (bytes, ..) = camera_jpeg((640, 427), (160, 107), 1);
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(&[0; 29]);
    trailing.extend_from_slice(b"\xff\x00CR3 padding");
    assert_eq!(extract::through_last_eoi(&trailing), bytes.as_slice());
    assert_eq!(extract::through_last_eoi(&bytes), bytes.as_slice());
    let strict = (|| -> Result<(), luxforge_jpeg::JpegError> {
        let mut decoder = luxforge_jpeg::Decoder::new(&trailing, crate::source::JPEG_LIMITS)?;
        let mut rows = vec![0; decoder.width() as usize * decoder.height() as usize * 4];
        decoder.read_rows(&mut rows)?;
        decoder.finish()
    })();
    assert!(
        matches!(strict, Err(luxforge_jpeg::JpegError::Malformed(_))),
        "the codec refuses the untrimmed bytes: {strict:?}"
    );
    let mut fixture = Fixture::new("preview-cache-trailing");
    let path = fixture.file("IMG_0001.JPG", &trailing);
    let file = fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let preview = run(&mut fixture.store(), &task((file, PreviewTier::Loupe)))
        .0
        .result
        .unwrap();
    assert_eq!((preview.width, preview.height), (640, 427));
}

/// An embedded ICC profile is kept.
#[test]
fn preview_cache_a_tier_keeps_the_files_icc_profile() {
    let source = paths::fixture("s0/srgb.jpg");
    let bytes = fs::read(&source).unwrap();
    let profile = luxforge_jpeg::Decoder::new(&bytes, crate::source::JPEG_LIMITS)
        .unwrap()
        .icc_profile()
        .map(<[u8]>::to_vec)
        .expect("the fixture carries a profile");
    let mut fixture = Fixture::new("preview-cache-icc");
    let path = fixture.file("srgb.jpg", &bytes);
    let file = fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let preview = run(&mut fixture.store(), &task((file, PreviewTier::Grid)))
        .0
        .result
        .unwrap();
    let written = fs::read(&preview.path).unwrap();
    let decoder = luxforge_jpeg::Decoder::new(&written, crate::source::JPEG_LIMITS).unwrap();
    assert_eq!(decoder.icc_profile(), Some(profile.as_slice()));
}

/// A file changed since it was indexed is refused before anything is read or written; a missing
/// one is unavailable, and its cached tiers stay served.
#[test]
fn preview_cache_a_changed_or_missing_file_is_refused_and_nothing_written() {
    let mut fixture = Fixture::new("preview-cache-changed");
    let (bytes, ..) = camera_jpeg((640, 427), (160, 107), 1);
    let path = fixture.file("A.JPG", &bytes);
    let file = fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let mut store = fixture.store();
    let grid = run(&mut store, &task((file, PreviewTier::Grid)))
        .0
        .result
        .unwrap();

    fs::write(&path, [bytes.as_slice(), b"more"].concat()).unwrap();
    let outcome = run(&mut store, &task((file, PreviewTier::Loupe))).0;
    let error = outcome.result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert!(
        error.detail.contains("changed since it was indexed"),
        "{}",
        error.detail
    );
    assert!(
        !outcome.permanent,
        "a changed file is read again once it is indexed again"
    );
    assert_eq!(fixture.tiers(file).loupe, None);
    let written: Vec<_> = walk(&fixture.index.previews_dir());
    assert_eq!(
        written,
        std::slice::from_ref(&grid.path),
        "nothing else was written"
    );

    fs::write(&path, &bytes).unwrap();
    fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let grid = run(&mut store, &task((file, PreviewTier::Grid)))
        .0
        .result
        .unwrap();
    fs::remove_file(&path).unwrap();
    let error = run(&mut store, &task((file, PreviewTier::Loupe)))
        .0
        .result
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        fixture.tiers(file).grid.map(|grid| grid.info()),
        Some(grid),
        "an unavailable file's cached tiers stay"
    );
}

/// A file that changes while its previews are read is checked again before anything is written:
/// what was made from it is discarded, and the tier is not written.
#[test]
fn preview_cache_a_file_changed_while_read_is_discarded() {
    let mut fixture = Fixture::new("preview-cache-changed-while-read");
    let (bytes, offset, len) = camera_jpeg((640, 427), (160, 107), 1);
    let path = fixture.file("A.JPG", &bytes);
    let file = fixture.add(
        &path,
        SourceTag::Jpeg,
        header(1, Some((offset, len, (160, 107)))),
    );
    // The file is rewritten once the thumbnail stage is written, while the embedded stage is read.
    let outcome = lane::run(
        &mut fixture.store(),
        &task((file, PreviewTier::Grid)),
        &|| {
            fs::write(&path, [bytes.as_slice(), b"edited"].concat()).unwrap();
        },
    );
    let error = outcome.result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert!(
        error
            .detail
            .contains("changed while its previews were read"),
        "{}",
        error.detail
    );
    assert!(!outcome.permanent);
    let grid = fixture
        .tiers(file)
        .grid
        .expect("the stage written before the change");
    assert_eq!(
        grid.origin,
        PreviewOrigin::ExifThumbnail,
        "no embedded stage was written"
    );
}

/// Every file under `dir`, sorted.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A JPEG cut short: its first half and an EOI marker.
pub(crate) fn broken_jpeg() -> Vec<u8> {
    let (bytes, ..) = camera_jpeg((640, 427), (160, 107), 1);
    let mut broken = bytes[..bytes.len() / 2].to_vec();
    broken.extend_from_slice(&[0xff, 0xd9]);
    broken
}

/// A corrupt JPEG original fails with its own kind, `invalid-input`, naming the file — never
/// relabelled as unsupported — and the lane is told to remember it; nothing is developed.
#[test]
fn preview_cache_a_corrupt_jpeg_is_invalid_input_and_remembered() {
    let mut fixture = Fixture::new("preview-cache-unusable");
    let path = fixture.file("BROKEN.JPG", &broken_jpeg());
    let file = fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending);
    let mut task = task((file, PreviewTier::Grid));
    task.develop = Some(std::sync::Arc::new(
        |_: &Path,
         _: &FileSignature,
         _: u32,
         _: &crate::Cancel|
         -> Result<super::region::DevelopedPreview, Error> {
            panic!("a JPEG original is never developed")
        },
    ));
    let outcome = run(&mut fixture.store(), &task).0;
    let error = outcome.result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Decode);
    assert_eq!(error.kind.code(), "invalid-input");
    assert!(
        error.detail.contains("BROKEN.JPG does not decode"),
        "{}",
        error.detail
    );
    assert!(outcome.permanent);
    assert!(!outcome.deferred);
    assert!(outcome.signature.is_some());
    let previews = fixture.index.previews_dir();
    assert!(
        !previews.exists() || walk(&previews).is_empty(),
        "nothing was written"
    );
}

/// A minimal uncompressed 16-bit RGGB DNG, `width` × `height`, that LibRaw identifies as `make`
/// `model` and that carries no embedded preview at all: a RAW with no usable preview, as the Canon
/// EOS R5 Mark II's and R8's H.265-only files are to the lane. No camera mode of the RAW catalog
/// has its size, so it never develops.
pub(crate) fn synthetic_dng(make: &str, model: &str, width: u32, height: u32) -> Vec<u8> {
    let text = |value: &str| {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        bytes
    };
    let (make, model) = (text(make), text(model));
    // Tag, type, count, inline value or payload, sorted by tag.
    let entries: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (254, 4, 1, 0_u32.to_le_bytes().to_vec()),
        (256, 4, 1, width.to_le_bytes().to_vec()),
        (257, 4, 1, height.to_le_bytes().to_vec()),
        (258, 3, 1, 16_u32.to_le_bytes().to_vec()),
        (259, 3, 1, 1_u32.to_le_bytes().to_vec()),
        (262, 3, 1, 32_803_u32.to_le_bytes().to_vec()),
        (271, 2, make.len() as u32, make),
        (272, 2, model.len() as u32, model),
        (273, 4, 1, Vec::new()),
        (277, 3, 1, 1_u32.to_le_bytes().to_vec()),
        (278, 4, 1, height.to_le_bytes().to_vec()),
        (279, 4, 1, (width * height * 2).to_le_bytes().to_vec()),
        (284, 3, 1, 1_u32.to_le_bytes().to_vec()),
        (33_421, 3, 2, vec![2, 0, 2, 0]),
        (33_422, 1, 4, vec![0, 1, 1, 2]),
        (50_706, 1, 4, vec![1, 4, 0, 0]),
        (50_717, 4, 1, 65_535_u32.to_le_bytes().to_vec()),
    ];
    let mut payload = 8 + 2 + entries.len() * 12 + 4;
    let strings: usize = entries
        .iter()
        .filter(|entry| entry.3.len() > 4)
        .map(|entry| entry.3.len())
        .sum();
    let data_offset = (payload + strings).next_multiple_of(2) as u32;
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut tail = Vec::new();
    for (tag, kind, count, value) in &entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if *tag == 273 {
            out.extend_from_slice(&data_offset.to_le_bytes());
        } else if value.len() > 4 {
            out.extend_from_slice(&(payload as u32).to_le_bytes());
            payload += value.len();
            tail.extend_from_slice(value);
        } else {
            let mut inline = [0_u8; 4];
            inline[..value.len()].copy_from_slice(value);
            out.extend_from_slice(&inline);
        }
    }
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&tail);
    out.resize(data_offset as usize, 0);
    for index in 0..width * height {
        out.extend_from_slice(&((index * 37 % 60_000) as u16).to_le_bytes());
    }
    out
}

/// A development standing in for a neutral one, counting itself in `count`: a grey frame of
/// 96 × 64 fitted within the side asked for, as `region::developed_preview` fits its own.
pub(crate) fn counted_development(
    count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> lane::DevelopHook {
    std::sync::Arc::new(
        move |_: &Path,
              _: &FileSignature,
              side: u32,
              _: &crate::Cancel|
              -> Result<super::region::DevelopedPreview, Error> {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (width, height) = if side >= 96 {
                (96, 64)
            } else {
                (side, (64 * side).div_ceil(96))
            };
            Ok(super::region::DevelopedPreview {
                width,
                height,
                rgba: std::sync::Arc::new(vec![128; (width * height * 4) as usize]),
                frame: super::region::FrameSize {
                    width: 96,
                    height: 64,
                },
            })
        },
    )
}

/// A RAW that carries no usable preview is developed at the seam for a task that may develop,
/// labelled `developed` and written; a background task defers it instead — `not-ready`, not
/// remembered as a failure, nothing developed or written; a development that fails keeps its
/// kind, and one Luxforge cannot make is remembered.
#[test]
fn preview_cache_a_raw_with_no_usable_preview_is_developed_or_deferred() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut fixture = Fixture::new("preview-cache-develop");
    let path = fixture.file("H265.DNG", &synthetic_dng("DJI", "FC3411", 64, 48));
    let file = fixture.add(&path, SourceTag::Raw, HeaderState::Pending);
    let mut store = fixture.store();
    let count = Arc::new(AtomicUsize::new(0));

    let mut background = task((file, PreviewTier::Grid));
    background.develops = false;
    background.develop = Some(counted_development(count.clone()));
    let (outcome, stages) = run(&mut store, &background);
    let error = outcome.result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotReady);
    assert!(
        error.detail.contains("H265.DNG has no usable preview"),
        "{}",
        error.detail
    );
    assert!(outcome.deferred);
    assert!(!outcome.permanent);
    assert_eq!(stages, 0, "the file carries no thumbnail");
    assert_eq!(count.load(Ordering::SeqCst), 0, "nothing was developed");
    assert_eq!(fixture.tiers(file).grid, None);

    for (tier, side) in [(PreviewTier::Grid, 96), (PreviewTier::Loupe, 96)] {
        let mut visible = task((file, tier));
        visible.develop = Some(counted_development(count.clone()));
        let made = run(&mut store, &visible)
            .0
            .result
            .unwrap_or_else(|error| panic!("{tier:?}: {error:?}"));
        assert_eq!(made.origin, PreviewOrigin::Developed);
        assert_eq!((made.width, made.height), (side, 64));
        assert_eq!(decoded(&made.path).dimensions(), (side, 64));
        assert!(made.key.ends_with(":developed"), "{}", made.key);
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);

    // A camera the development cannot handle is remembered; a development that could not read the
    // file is tried again.
    let other = fixture.file("OTHER.DNG", &synthetic_dng("DJI", "FC3411", 64, 48));
    let other = fixture.add(&other, SourceTag::Raw, HeaderState::Pending);
    for (error, lasting) in [
        (Error::unsupported_input("outside the RAW catalog"), true),
        (Error::file_access("interrupted"), false),
    ] {
        let kind = error.kind;
        let mut failing = task((other, PreviewTier::Grid));
        failing.develop = Some(Arc::new(
            move |_: &Path,
                  _: &FileSignature,
                  _: u32,
                  _: &crate::Cancel|
                  -> Result<super::region::DevelopedPreview, Error> {
                Err(error.clone())
            },
        ));
        let outcome = run(&mut store, &failing).0;
        let failed = outcome.result.unwrap_err();
        assert_eq!(failed.kind, kind);
        assert!(
            failed.detail.contains("OTHER.DNG has no usable preview")
                && failed.detail.contains("cannot be developed"),
            "{}",
            failed.detail
        );
        assert_eq!(outcome.permanent, lasting, "{kind:?}");
        assert!(!outcome.deferred);
    }
}

/// A cancelled task stops and writes nothing.
#[test]
fn preview_cache_a_cancelled_task_writes_nothing() {
    let mut fixture = Fixture::new("preview-cache-cancel");
    let (bytes, offset, len) = camera_jpeg((640, 427), (160, 107), 1);
    let path = fixture.file("A.JPG", &bytes);
    let file = fixture.add(
        &path,
        SourceTag::Jpeg,
        header(1, Some((offset, len, (160, 107)))),
    );
    let task = task((file, PreviewTier::Grid));
    task.control.cancel("stopped by a client");
    let outcome = run(&mut fixture.store(), &task).0;
    assert_eq!(outcome.result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(!outcome.permanent);
    assert_eq!(fixture.tiers(file).grid, None);
}

/// Loupe tiers share the budget with developed photographs' large tiers, least recently used out
/// first and never the one just written; grid tiers are kept whatever the budget. A served loupe's
/// use is recorded at most once a minute.
#[test]
fn preview_cache_the_loupe_budget_evicts_the_least_recently_used_and_keeps_grids() {
    let mut fixture = Fixture::new("preview-cache-budget");
    let mut store = fixture.store();
    let mut files = Vec::new();
    for index in 0..4 {
        let (bytes, ..) = camera_jpeg((600 + index * 10, 400), (160, 107), 1);
        let path = fixture.file(&format!("L{index}.JPG"), &bytes);
        files.push(fixture.add(&path, SourceTag::Jpeg, HeaderState::Pending));
    }
    let grids: Vec<_> = files
        .iter()
        .map(|file| {
            run(&mut store, &task((*file, PreviewTier::Grid)))
                .0
                .result
                .unwrap()
        })
        .collect();
    let mut loupes = Vec::new();
    for file in &files[..3] {
        loupes.push(
            run(&mut store, &task((*file, PreviewTier::Loupe)))
                .0
                .result
                .unwrap(),
        );
    }
    // Ages the three loupes apart: the second is the oldest, then the first, then the third.
    for (file, used) in [(files[0], 2_000), (files[1], 1_000), (files[2], 3_000)] {
        fixture
            .index
            .connection()
            .execute(
                "UPDATE previews SET last_used_ms = ?2 WHERE file_id = ?1 AND tier = 'loupe'",
                rusqlite::params![file.0, used],
            )
            .unwrap();
    }
    // A developed photograph's large tier, used before all of them, shares the budget.
    let large = fixture.index.previews_dir().join("photos/large.jpg");
    fs::create_dir_all(large.parent().unwrap()).unwrap();
    fs::write(&large, [1; 100]).unwrap();
    fixture
        .index
        .connection()
        .execute(
            "INSERT INTO photo_previews VALUES ('asset-1', 'entry-1', 'large', 1, ?1, 10, 10, 100,
                 'rendered', 500)",
            [large.to_string_lossy()],
        )
        .unwrap();
    let sizes: Vec<u64> = loupes.iter().map(|loupe| loupe.bytes).collect();
    // Room for the new loupe and the third: the large tier, the second and the first go, oldest
    // first.
    let mut budget_task = task((files[3], PreviewTier::Loupe));
    budget_task.budget = sizes[2] * 3;
    run(&mut store, &budget_task).0.result.unwrap();
    let loupe_rows = |file: FileId| fixture.tiers(file).loupe;
    assert!(
        !large.exists(),
        "the large tier, least recently used, went first"
    );
    assert_eq!(loupe_rows(files[1]), None, "then the second loupe");
    assert!(!loupes[1].path.exists());
    assert!(
        loupe_rows(files[2]).is_some(),
        "the most recently used stays"
    );
    assert!(loupe_rows(files[3]).is_some(), "the one just written stays");
    let total: u64 = [files[0], files[2], files[3]]
        .iter()
        .filter_map(|file| loupe_rows(*file))
        .map(|loupe| loupe.bytes)
        .sum();
    assert!(
        total <= budget_task.budget,
        "{total} within {}",
        budget_task.budget
    );
    for (file, grid) in files.iter().zip(&grids) {
        assert_eq!(
            fixture.tiers(*file).grid.unwrap().info(),
            *grid,
            "grids are kept"
        );
    }

    // A served loupe's use is recorded, at most once a minute.
    let used = |file: FileId| fixture.tiers(file).loupe.unwrap().last_used_ms;
    cache::touch(fixture.index.connection(), files[2], 100_000).unwrap();
    assert_eq!(used(files[2]), 100_000);
    cache::touch(fixture.index.connection(), files[2], 100_000 + 59_999).unwrap();
    assert_eq!(used(files[2]), 100_000, "not again within the minute");
    cache::touch(fixture.index.connection(), files[2], 160_000).unwrap();
    assert_eq!(used(files[2]), 160_000);
}

/// The grid states lane D reads and the sizes lane C reads, one query each.
#[test]
fn preview_cache_grid_states_and_cache_bytes() {
    let mut fixture = Fixture::new("preview-cache-states");
    let (bytes, offset, len) = camera_jpeg((640, 427), (160, 107), 1);
    let ready = fixture.add(
        &fixture.file("A.JPG", &bytes),
        SourceTag::Jpeg,
        HeaderState::Pending,
    );
    let staged = fixture.add(
        &fixture.file("B.JPG", &bytes),
        SourceTag::Jpeg,
        header(1, Some((offset, len, (160, 107)))),
    );
    let pending = fixture.add(
        &fixture.file("C.JPG", &bytes),
        SourceTag::Jpeg,
        HeaderState::Pending,
    );
    let mut store = fixture.store();
    let grid = run(&mut store, &task((ready, PreviewTier::Grid)))
        .0
        .result
        .unwrap();
    let loupe = run(&mut store, &task((ready, PreviewTier::Loupe)))
        .0
        .result
        .unwrap();
    // The thumbnail stage alone: the task stops after it.
    let stopped = task((staged, PreviewTier::Grid));
    let control = stopped.control.clone();
    let outcome = lane::run(&mut store, &stopped, &|| {
        control.cancel("held after the stage")
    });
    assert_eq!(outcome.result.unwrap_err().kind, ErrorKind::Cancelled);
    let thumbnail = fixture.tiers(staged).grid.unwrap();
    assert_eq!(
        cache::grid_states(
            fixture.index.connection(),
            &[pending, staged, ready, FileId(9_999), ready]
        )
        .unwrap(),
        [
            PreviewState::Pending,
            PreviewState::Thumbnail,
            PreviewState::Ready,
            PreviewState::Pending,
            PreviewState::Ready
        ]
    );
    let wanted = cache::grids_wanted(
        fixture.index.connection(),
        &[pending, staged, ready, FileId(9_999), pending],
    )
    .unwrap();
    assert_eq!(
        wanted.iter().map(|(file, _)| *file).collect::<Vec<_>>(),
        [pending, staged],
        "files without a complete grid, once each, and none the index does not hold"
    );
    let bytes = cache::cache_bytes(fixture.index.connection()).unwrap();
    assert_eq!(bytes.grid, grid.bytes + thumbnail.bytes);
    assert_eq!(bytes.loupe, loupe.bytes);
    assert_eq!(bytes.large, 0);
    assert_eq!(bytes.files, 3);
    assert_eq!(bytes.total(), grid.bytes + thumbnail.bytes + loupe.bytes);
}

/// The queue hands out the look-ahead first, then visible cells newest first, then the rest oldest
/// first; a second request joins the task and raises it; past the capacity a new task is refused.
#[test]
fn preview_cache_the_queue_orders_by_priority_and_deduplicates() {
    let key = |id: i64| (FileId(id), PreviewTier::Grid);
    let mut queue = Queue::default();
    assert_eq!(
        queue.push(key(1), PreviewPriority::Background).unwrap(),
        Pushed::New
    );
    assert_eq!(
        queue.push(key(2), PreviewPriority::Background).unwrap(),
        Pushed::New
    );
    assert_eq!(
        queue.push(key(3), PreviewPriority::Visible).unwrap(),
        Pushed::New
    );
    assert_eq!(
        queue.push(key(4), PreviewPriority::Visible).unwrap(),
        Pushed::New
    );
    assert_eq!(
        queue.push(key(5), PreviewPriority::LookAhead).unwrap(),
        Pushed::New
    );
    assert_eq!(
        queue.push(key(6), PreviewPriority::LookAhead).unwrap(),
        Pushed::New
    );
    assert_eq!(
        queue.push(key(1), PreviewPriority::Background).unwrap(),
        Pushed::Joined,
        "the same request joins and keeps its turn"
    );
    assert_eq!(
        queue.push(key(5), PreviewPriority::Visible).unwrap(),
        Pushed::Joined,
        "a lower priority never lowers a task"
    );
    assert_eq!(
        queue.push(key(2), PreviewPriority::LookAhead).unwrap(),
        Pushed::Raised
    );
    assert_eq!(
        queue.push(key(3), PreviewPriority::Visible).unwrap(),
        Pushed::Raised,
        "a visible request is the newest scroll"
    );
    assert_eq!(queue.len(), 6);
    let mut order = Vec::new();
    while let Some(((file, _), priority)) = queue.pop() {
        order.push((file.0, priority));
    }
    assert_eq!(
        order,
        [
            (5, PreviewPriority::LookAhead),
            (6, PreviewPriority::LookAhead),
            (2, PreviewPriority::LookAhead),
            (3, PreviewPriority::Visible),
            (4, PreviewPriority::Visible),
            (1, PreviewPriority::Background),
        ]
    );

    let mut queue = Queue::with_capacity(2);
    queue.push(key(1), PreviewPriority::Visible).unwrap();
    queue.push(key(2), PreviewPriority::Visible).unwrap();
    assert!(!queue.fits(1));
    let error = queue.push(key(3), PreviewPriority::LookAhead).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        queue.push(key(2), PreviewPriority::LookAhead).unwrap(),
        Pushed::Raised,
        "a queued task still joins at the bound"
    );
    assert!(queue.remove(&key(1)));
    assert!(!queue.remove(&key(1)));
    assert_eq!(queue.pop().map(|(key, _)| key.0), Some(FileId(2)));
    assert_eq!(PREVIEW_QUEUE_CAPACITY, 20_000);
}

/// A failure is remembered for the signature it happened at, the oldest forgotten first past the
/// bound.
#[test]
fn preview_cache_failures_are_remembered_per_signature() {
    let signature = FileSignature {
        len: 1,
        modified_ns: 2,
        identity: None,
    };
    let changed = FileSignature {
        len: 3,
        ..signature
    };
    let key = (FileId(1), PreviewTier::Grid);
    let mut failures = Failures::default();
    failures.remember(key, signature, Error::unsupported_input("H.265 only"));
    assert_eq!(failures.get(&key, &signature).unwrap().detail, "H.265 only");
    assert!(
        failures.get(&key, &changed).is_none(),
        "a changed file is tried again"
    );
    assert!(
        failures
            .get(&(FileId(1), PreviewTier::Loupe), &signature)
            .is_none()
    );
    failures.forget(&key);
    assert!(failures.get(&key, &signature).is_none());
    for id in 0..lane::REMEMBERED_FAILURES as i64 + 1 {
        failures.remember(
            (FileId(id), PreviewTier::Grid),
            signature,
            Error::unsupported_input("none"),
        );
    }
    assert!(
        failures
            .get(&(FileId(0), PreviewTier::Grid), &signature)
            .is_none()
    );
    assert!(
        failures
            .get(&(FileId(1), PreviewTier::Grid), &signature)
            .is_some()
    );
}

/// The generated image folders (`cargo xtask generate-catalog --images N`): every file's grid
/// arrives as its 160 × 107 thumbnail, then its 512 px embedded preview, and its loupe at its own
/// 640 × 427. Run with `LUXFORGE_GENERATED_CATALOG` naming the generator's output directory.
#[test]
#[ignore = "needs LUXFORGE_GENERATED_CATALOG: the output of cargo xtask generate-catalog --images N"]
fn preview_cache_the_generated_image_folders() {
    let out = PathBuf::from(
        std::env::var_os("LUXFORGE_GENERATED_CATALOG")
            .expect("LUXFORGE_GENERATED_CATALOG names the generator's output"),
    );
    let images = out.join("images");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(images.join("manifest.json")).unwrap()).unwrap();
    let thumbnail = (
        manifest["thumbnail"]["width"].as_u64().unwrap() as u32,
        manifest["thumbnail"]["height"].as_u64().unwrap() as u32,
    );
    let size = (
        manifest["image"]["width"].as_u64().unwrap() as u32,
        manifest["image"]["height"].as_u64().unwrap() as u32,
    );
    let mut fixture = Fixture::new("preview-cache-generated");
    let mut store = fixture.store();
    let files = manifest["files"].as_array().unwrap();
    for entry in files {
        let path = images.join(entry["path"].as_str().unwrap());
        let bytes = fs::read(&path).unwrap();
        let (offset, len, orientation) = ifd1_thumbnail(&bytes);
        let file = fixture.add(
            &path,
            SourceTag::Jpeg,
            header(orientation, Some((offset, len, thumbnail))),
        );
        let stage = std::cell::RefCell::new(None);
        let grid = lane::run(&mut store, &task((file, PreviewTier::Grid)), &|| {
            *stage.borrow_mut() = fixture.tiers(file).grid;
        })
        .result
        .unwrap();
        let stage = stage.into_inner().expect("a thumbnail stage");
        assert_eq!(stage.origin, PreviewOrigin::ExifThumbnail);
        let edges = |(width, height): (u32, u32)| (width.max(height), width.min(height));
        assert_eq!(
            edges((stage.width, stage.height)),
            edges(thumbnail),
            "{path:?}"
        );
        assert_eq!(grid.origin, PreviewOrigin::Embedded);
        let long = size.0.max(size.1).min(FILE_GRID_SIDE);
        assert_eq!(grid.width.max(grid.height), long, "{path:?}");
        let loupe = run(&mut store, &task((file, PreviewTier::Loupe)))
            .0
            .result
            .unwrap();
        assert_eq!(edges((loupe.width, loupe.height)), edges(size));
        assert_eq!(fs::read(&path).unwrap(), bytes, "the original is untouched");
    }
    println!(
        "{} generated files: grid {}×{} via a {}×{} thumbnail stage, loupe {}×{}",
        files.len(),
        512,
        (f64::from(size.1) * 512.0 / f64::from(size.0)).round(),
        thumbnail.0,
        thumbnail.1,
        size.0,
        size.1
    );
}

/// Where IFD1's JPEG thumbnail is in a JPEG file, found by the independent EXIF reader (its
/// offsets count from the TIFF header after the APP1's `Exif\0\0`), and the file's orientation.
fn ifd1_thumbnail(bytes: &[u8]) -> (u64, u32, u8) {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .unwrap();
    let value = |tag| {
        exif.get_field(tag, In::THUMBNAIL)
            .and_then(|field| field.value.get_uint(0))
            .expect("IFD1 locates its thumbnail")
    };
    let tiff = bytes
        .windows(6)
        .position(|window| window == b"Exif\0\0")
        .unwrap()
        + 6;
    let orientation = exif
        .get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .map_or(1, |value| value as u8);
    (
        tiff as u64 + u64::from(value(Tag::JPEGInterchangeFormat)),
        value(Tag::JPEGInterchangeFormatLength),
        orientation,
    )
}

/// The owner's authentic RAW files make grid and loupe tiers with the right origins and sizes, and
/// are never changed. Run with `LUXFORGE_RAW_OWNER_DIR` naming the folder that holds
/// `nikon_z6.NEF`, `fujifilm_x100vi.RAF` and `mavic_air_2s.DNG`.
#[test]
#[ignore = "needs LUXFORGE_RAW_OWNER_DIR: the owner's RAW files, outside Git"]
fn preview_cache_authentic_raw_files() {
    use sha2::{Digest, Sha256};
    let dir = PathBuf::from(
        std::env::var_os("LUXFORGE_RAW_OWNER_DIR")
            .expect("LUXFORGE_RAW_OWNER_DIR names the owner's RAW folder"),
    );
    // Each camera's largest embedded preview (docs/research/embedded-previews.md) and whether it
    // carries a thumbnail besides it.
    let cameras = [
        ("nikon_z6.NEF", (6048, 4024), true),
        ("fujifilm_x100vi.RAF", (4416, 2944), true),
        ("mavic_air_2s.DNG", (960, 640), true),
    ];
    let mut fixture = Fixture::new("preview-cache-raw");
    let mut store = fixture.store();
    for (name, largest, has_thumbnail) in cameras {
        let path = dir.join(name);
        let before = Sha256::digest(fs::read(&path).unwrap());
        let file = fixture.add(&path, SourceTag::Raw, HeaderState::Pending);
        let stage = std::cell::RefCell::new(None);
        let grid = lane::run(&mut store, &task((file, PreviewTier::Grid)), &|| {
            *stage.borrow_mut() = fixture.tiers(file).grid;
        })
        .result
        .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let loupe = run(&mut store, &task((file, PreviewTier::Loupe)))
            .0
            .result
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let stage = stage.into_inner();
        assert_eq!(stage.is_some(), has_thumbnail, "{name}");
        if let Some(stage) = &stage {
            assert_eq!(stage.origin, PreviewOrigin::ExifThumbnail, "{name}");
            assert!(stage.width.max(stage.height) <= FILE_GRID_SIDE, "{name}");
        }
        for (preview, side) in [(&grid, FILE_GRID_SIDE), (&loupe, LOUPE_MAX_SIDE)] {
            assert_eq!(preview.origin, PreviewOrigin::Embedded, "{name}");
            let long = largest.0.max(largest.1);
            let short = largest.0.min(largest.1);
            let fitted_long = long.min(side);
            let fitted_short =
                (f64::from(short) * f64::from(fitted_long) / f64::from(long)).round() as u32;
            let (width, height) = (preview.width, preview.height);
            assert_eq!(
                (width.max(height), width.min(height)),
                (fitted_long, fitted_short),
                "{name} {:?}",
                preview.tier
            );
            assert_eq!(decoded(&preview.path).dimensions(), (width, height));
        }
        let after = Sha256::digest(fs::read(&path).unwrap());
        assert_eq!(before, after, "{name} is unchanged");
        // Keep the tiers for a look, when asked.
        if let Some(out) = std::env::var_os("LUXFORGE_PREVIEW_OUT") {
            let out = PathBuf::from(out);
            fs::create_dir_all(&out).unwrap();
            for preview in [&grid, &loupe] {
                fs::copy(
                    &preview.path,
                    out.join(format!("{name}-{}.jpg", preview.tier.as_str())),
                )
                .unwrap();
            }
        }
        println!(
            "{name}: thumbnail stage {:?}, grid {}×{}, loupe {}×{} ({} bytes)",
            stage.map(|stage| (stage.width, stage.height)),
            grid.width,
            grid.height,
            loupe.width,
            loupe.height,
            loupe.bytes
        );
    }
}
