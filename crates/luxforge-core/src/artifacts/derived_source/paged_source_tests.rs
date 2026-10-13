//! Paged derived sources: the pyramid's shapes and bounds, exact round trips at level 0 and exact
//! area averages at every level held to an independent reduction, windows equal to the region of
//! the whole level they cut, preparation that reads the overview alone, damage that fails only
//! what reads it by name, write failures at each level, manifests of other shapes refused and kept,
//! collection of every level, and the chunk cache's bounds and its charge on the plane gate.
use super::super::{
    LiveSources, MAX_SOURCE_MANIFEST_BYTES, PreparedDerived, SOURCES, Step, TEMPORARY,
    collect_source_objects, fail, payload_work, prepare_derived, read_derived_source,
    read_manifest, source_object_path, stored_chunks,
    tests::{planes, same_bits, sample},
};
use super::*;
use crate::{
    ErrorKind,
    source::{GATE_ADMITTED_BYTES, PlaneGate},
};
use luxforge_test_macros::slow;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "luxforge-paged-source-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

fn writer(root: &Path, live: &LiveSources) -> DerivedSourceWriter {
    DerivedSourceWriter::new(root.to_path_buf(), "catalog-a".into(), live.clone())
}

fn reading(root: &Path, published: &PublishedSource) -> DerivedSourceRead {
    DerivedSourceRead {
        root: root.to_path_buf(),
        catalog_id: "catalog-a".into(),
        id: published.id.clone(),
        manifest_bytes: published.manifest_bytes,
    }
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn open(root: &Path, published: &PublishedSource) -> PagedSource {
    match prepare_derived(&reading(root, published), &never()).unwrap() {
        PreparedDerived::Paged(source) => *source,
        PreparedDerived::Resident(_) => panic!("a paged source prepares paged"),
    }
}

fn objects(root: &Path) -> HashSet<String> {
    match fs::read_dir(root.join(SOURCES)) {
        Err(_) => HashSet::new(),
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect(),
    }
}

fn staged(root: &Path) -> usize {
    fs::read_dir(root.join(TEMPORARY)).map_or(0, |entries| entries.count())
}

/// One level of a reference pyramid: whole planes, worked out here apart from the writer.
struct Level {
    width: u32,
    height: u32,
    planes: Vec<f32>,
}

/// The mean the declared reduction gives a block, written out again: the block's samples summed in
/// binary64 in the order given, from the first, and rounded once to binary32.
fn mean(samples: &[f32]) -> f32 {
    let mut sum = f64::from(samples[0]);
    for &sample in &samples[1..] {
        sum += f64::from(sample);
    }
    (sum / samples.len() as f64) as f32
}

/// The next level of `level`: each pixel the mean of the up to four pixels it covers, top-left,
/// top-right, bottom-left, bottom-right.
fn reduce(level: &Level) -> Level {
    let (width, height) = (level.width.div_ceil(2), level.height.div_ceil(2));
    let from = level.width as usize * level.height as usize;
    let to = width as usize * height as usize;
    let mut planes = vec![0.0_f32; 3 * to];
    let mut block = Vec::with_capacity(4);
    for channel in 0..3 {
        for y in 0..height {
            for x in 0..width {
                block.clear();
                for (sx, sy) in [
                    (2 * x, 2 * y),
                    (2 * x + 1, 2 * y),
                    (2 * x, 2 * y + 1),
                    (2 * x + 1, 2 * y + 1),
                ] {
                    if sx < level.width && sy < level.height {
                        block.push(level.planes[channel * from + (sy * level.width + sx) as usize]);
                    }
                }
                planes[channel * to + (y * width + x) as usize] = mean(&block);
            }
        }
    }
    Level {
        width,
        height,
        planes,
    }
}

/// The reference pyramid of whole planes `planes`, `width` × `height`, down to the overview.
fn pyramid(width: u32, height: u32, planes: Vec<f32>) -> Vec<Level> {
    let mut levels = vec![Level {
        width,
        height,
        planes,
    }];
    loop {
        let last = levels.last().unwrap();
        if last.width.max(last.height) <= OVERVIEW_LONG_SIDE {
            return levels;
        }
        let next = reduce(last);
        levels.push(next);
    }
}

/// The rectangle `rect` cut out of a level's whole planes.
fn cut(level: &Level, rect: [u32; 4]) -> Vec<f32> {
    let [x, y, width, height] = rect;
    let plane = level.width as usize * level.height as usize;
    let mut out = Vec::with_capacity(3 * width as usize * height as usize);
    for channel in 0..3 {
        for row in y..y + height {
            let at = channel * plane + (row * level.width + x) as usize;
            out.extend_from_slice(&level.planes[at..at + width as usize]);
        }
    }
    out
}

fn window(source: &PagedSource, level: usize, rect: [u32; 4], halo: u32) -> PagedWindow {
    source
        .window(WindowRequest { level, rect, halo }, &never())
        .unwrap()
}

fn whole(source: &PagedSource, level: usize) -> PagedWindow {
    let (width, height) = source.levels()[level];
    window(source, level, [0, 0, width, height], 0)
}

/// The small paged source most tests use: wider than the camera side by six pixels and five rows,
/// odd in both directions, so every level has right or bottom edges averaging fewer than four.
const WIDTH: u32 = 16_390;
const HEIGHT: u32 = 5;
/// Its stored chunks, level by level: 17, 9, 5 and 3.
const LEVEL_CHUNKS: [usize; 4] = [17, 9, 5, 3];

fn small(root: &Path, live: &LiveSources) -> (PublishedSource, Vec<f32>) {
    let values = planes(WIDTH, HEIGHT);
    let published = writer(root, live)
        .write_planes(WIDTH, HEIGHT, &values, None, &never())
        .unwrap();
    (published, values)
}

#[test]
fn paged_source_levels_classes_and_bounds() {
    assert_eq!(
        level_shapes(65_000, 7_876),
        [
            (65_000, 7_876),
            (32_500, 3_938),
            (16_250, 1_969),
            (8_125, 985),
            (4_063, 493)
        ]
    );
    assert_eq!(
        level_shapes(WIDTH, HEIGHT),
        [(16_390, 5), (8_195, 3), (4_098, 2), (2_049, 1)]
    );
    for (width, height, class) in [
        (16_384, 7_812, Ok(SourceClass::Resident)),
        (1, 1, Ok(SourceClass::Resident)),
        (16_385, 1, Ok(SourceClass::Paged)),
        // Within the camera side, past its pixels.
        (16_000, 9_000, Ok(SourceClass::Paged)),
        (65_000, 7_876, Ok(SourceClass::Paged)),
        (22_627, 22_627, Ok(SourceClass::Paged)),
        (65_001, 1, Err(ErrorKind::ResourceLimit)),
        (65_000, 7_877, Err(ErrorKind::ResourceLimit)),
        (0, 20_000, Err(ErrorKind::Validation)),
    ] {
        assert_eq!(
            SourceClass::of(width, height).map_err(|error| error.kind),
            class,
            "{width} × {height}"
        );
    }
    // The chunk bounds hold for the extreme shapes the paged limits admit, and the last level is
    // the first at or below the overview's long side.
    for (width, height) in [
        (65_000, 7_876),
        (22_627, 22_627),
        (65_000, 1),
        (16_385, 31_247),
    ] {
        let shapes = level_shapes(width, height);
        assert!(grid_of(width, height).len() <= MAX_LEVEL0_CHUNKS);
        assert!(stored_chunks(width, height).unwrap().len() <= MAX_PAGED_CHUNKS);
        let (last_w, last_h) = shapes[shapes.len() - 1];
        let (before_w, before_h) = shapes[shapes.len() - 2];
        assert!(last_w.max(last_h) <= OVERVIEW_LONG_SIDE);
        assert!(before_w.max(before_h) > OVERVIEW_LONG_SIDE);
    }
    assert_eq!(
        stored_chunks(65_000, 7_876).unwrap().len(),
        512 + 128 + 32 + 8 + 4
    );
    // The manifest bound holds the chunk bound: 5,462 entries of the longest numbers fit 1 MiB.
    let entry = ChunkEntry::new(
        ChunkRect {
            x: 64_512,
            y: 64_512,
            width: 1024,
            height: 1024,
        },
        "f".repeat(64),
    );
    let mut levels = vec![LevelManifest {
        width: 65_000,
        height: 65_000,
        chunks: vec![entry.clone(); MAX_LEVEL0_CHUNKS],
    }];
    let mut left = MAX_PAGED_CHUNKS - MAX_LEVEL0_CHUNKS;
    while left > 0 {
        let count = left.min(1024);
        levels.push(LevelManifest {
            width: 65_000,
            height: 65_000,
            chunks: vec![entry.clone(); count],
        });
        left -= count;
    }
    let worst = PagedManifest {
        format: PAGED_MANIFEST_FORMAT,
        width: 65_000,
        height: 65_000,
        colour: DerivedColour::LinearSrgbD65,
        sample: SampleEncoding::F32Le,
        layout: ChunkLayout::PlanarRgb,
        chunk_side: CHUNK_SIDE,
        reduction: Reduction::AreaAverage2x2,
        crop: Some(ReferenceCrop {
            reference_width: u32::MAX,
            reference_height: u32::MAX,
            x: u32::MAX,
            y: u32::MAX,
        }),
        levels,
    };
    let bytes = serde_json::to_vec(&worst).unwrap().len() as u64;
    assert!(bytes <= MAX_SOURCE_MANIFEST_BYTES, "{bytes}");
    // The declared reduction: the sum in binary64, in order, rounded once.
    let block = [1.0e-40_f32, 262_016.0, -0.0, 3.0];
    let expected = ((f64::from(1.0e-40_f32) + 262_016.0 + -0.0 + 3.0) / 4.0) as f32;
    assert_eq!(area_average(&block).to_bits(), expected.to_bits());
    assert_eq!(
        area_average(&[-0.0]).to_bits(),
        (-0.0_f32).to_bits(),
        "one pixel alone keeps its bits"
    );
}

/// A paged source is written as format 2 with every level, read back with exact bits at level 0
/// and exact area averages at every further level against the independent reduction; its
/// preparation reads the manifest and the overview alone; windows with halos equal the region of
/// the whole level they cut, clamped at the level's edges; the resident level is the largest
/// within the camera limits; and chunks put in any order make the same source.
#[test]
fn paged_source_round_trips_with_exact_levels_and_windows() {
    let directory = temp("round-trip");
    let root = directory.join("catalog.artifacts");
    let live = LiveSources::default();
    let values = planes(WIDTH, HEIGHT);
    let crop = ReferenceCrop {
        reference_width: WIDTH + 40,
        reference_height: HEIGHT + 2,
        x: 10,
        y: 1,
    };
    let published = writer(&root, &live)
        .write_planes(WIDTH, HEIGHT, &values, Some(crop), &never())
        .unwrap();
    published.validate().unwrap();
    assert_eq!(published.class().unwrap(), SourceClass::Paged);
    assert_eq!(published.chunks.len(), LEVEL_CHUNKS.iter().sum::<usize>());
    let reference = pyramid(WIDTH, HEIGHT, values.clone());
    let level_bytes: Vec<u64> = reference
        .iter()
        .map(|level| level.planes.len() as u64 * 4)
        .collect();
    assert_eq!(published.payload_bytes(), level_bytes.iter().sum::<u64>());
    // Every object named by its own hash and live; nothing staged.
    for name in objects(&root) {
        let bytes = fs::read(source_object_path(&root, &name)).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), name);
        assert!(live.contains(&name));
    }
    assert_eq!(staged(&root), 0);
    // The manifest: format 2, its reduction, every level and no top-level grid.
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(source_object_path(&root, published.id.sha256())).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["format"], 2);
    assert_eq!(manifest["reduction"], "area-average-2x2");
    assert_eq!(manifest["crop"]["x"], 10);
    assert!(manifest.get("chunks").is_none());
    let listed: Vec<(u64, u64, usize)> = manifest["levels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|level| {
            (
                level["width"].as_u64().unwrap(),
                level["height"].as_u64().unwrap(),
                level["chunks"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [(16_390, 5, 17), (8_195, 3, 9), (4_098, 2, 5), (2_049, 1, 3)]
    );
    let verified = read_manifest(&reading(&root, &published)).unwrap();
    assert_eq!(verified.class(), SourceClass::Paged);
    assert_eq!(verified.levels().len(), 4);
    assert_eq!(verified.crop(), Some(crop));
    assert!(
        verified
            .chunk_hashes()
            .eq(published.chunks.iter().map(|chunk| chunk.sha256.as_str()))
    );
    // A paged source has no whole frame to read.
    let error = read_derived_source(&reading(&root, &published), &never()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{error}");
    assert!(error.detail.contains("no whole frame"), "{error}");

    // Preparation: the manifest and the overview's three chunks, and nothing else.
    payload_work::take();
    let source = open(&root, &published);
    assert_eq!(payload_work::take(), 1 + 3);
    assert_eq!((source.width(), source.height()), (WIDTH, HEIGHT));
    assert_eq!(source.levels().len(), 4);
    assert_eq!(source.overview_level(), 3);
    assert_eq!(source.crop(), Some(crop));
    assert_eq!(source.colour(), DerivedColour::LinearSrgbD65);
    let overview = source.overview();
    assert_eq!((overview.width(), overview.height()), (2_049, 1));
    assert_eq!(overview.fingerprint(), format!("{}/level-3", published.id));
    assert!(same_bits(overview.planes(), &reference[3].planes));
    assert_eq!(source.cached_bytes(), (0, 0));

    // Every level whole: level 0 bit for bit, every further level the exact averages.
    for (level, expected) in reference.iter().enumerate() {
        let read = whole(&source, level);
        assert_eq!(read.rect, [0, 0, expected.width, expected.height]);
        assert!(same_bits(&read.planes, &expected.planes), "level {level}");
    }
    // Windows of every level with halos equal the region of the whole level they cut.
    for (level, rect, halo, read) in [
        (0, [1000, 1, 1100, 3], 7, [993, 0, 1114, 5]),
        (0, [16_380, 4, 10, 1], 2, [16_378, 2, 12, 3]),
        (0, [0, 0, 1, 1], 0, [0, 0, 1, 1]),
        (1, [1020, 0, 10, 3], 512, [508, 0, 1034, 3]),
        (2, [4090, 1, 8, 1], 3, [4087, 0, 11, 2]),
        (3, [2048, 0, 1, 1], 100, [1948, 0, 101, 1]),
    ] {
        let got = window(&source, level, rect, halo);
        assert_eq!((got.level, got.core, got.rect), (level, rect, read));
        assert!(
            same_bits(&got.planes, &cut(&reference[level], read)),
            "level {level} window {rect:?}"
        );
    }
    let bad = |level, rect| {
        source
            .window(
                WindowRequest {
                    level,
                    rect,
                    halo: 0,
                },
                &never(),
            )
            .unwrap_err()
            .kind
    };
    assert_eq!(bad(4, [0, 0, 1, 1]), ErrorKind::Validation);
    assert_eq!(bad(0, [16_390, 0, 1, 1]), ErrorKind::Validation);
    assert_eq!(bad(1, [0, 0, 0, 1]), ErrorKind::Validation);
    assert_eq!(bad(0, [u32::MAX, 0, 2, 1]), ErrorKind::Validation);
    let cancelled = source.window(
        WindowRequest {
            level: 0,
            rect: [0, 0, 10, 1],
            halo: 0,
        },
        &AtomicBool::new(true),
    );
    assert_eq!(cancelled.unwrap_err().kind, ErrorKind::Cancelled);
    // A window as an image keeps its bits.
    let image = window(&source, 2, [10, 0, 30, 2], 0)
        .into_image("window".into())
        .unwrap();
    assert!(same_bits(
        image.planes(),
        &cut(&reference[2], [10, 0, 30, 2])
    ));

    // The resident level: the largest within the camera limits, read once and held.
    assert_eq!(source.resident_level_index(), 1);
    let resident = source.resident_level(&never()).unwrap();
    assert_eq!(resident.fingerprint(), format!("{}/level-1", published.id));
    assert!(same_bits(resident.planes(), &reference[1].planes));
    payload_work::take();
    let again = source.resident_level(&never()).unwrap();
    assert_eq!(payload_work::take(), 0, "held once read");
    assert_eq!(again.development(), resident.development());

    // Level-0 chunks put in another order, as a producer may emit them, make the same source.
    let other = temp("round-trip-order");
    let other_root = other.join("catalog.artifacts");
    let mut write = writer(&other_root, &LiveSources::default())
        .begin_paged(WIDTH, HEIGHT, Some(crop), &never())
        .unwrap();
    let grid = write.grid().to_vec();
    assert_eq!(grid.len(), LEVEL_CHUNKS[0]);
    for (ordinal, rect) in grid.iter().enumerate().rev() {
        let chunk = cut(&reference[0], [rect.x, rect.y, rect.width, rect.height]);
        write.put(ordinal, &chunk, &never()).unwrap();
    }
    assert_eq!(write.finish(&never()).unwrap(), published);
    assert_eq!(objects(&other_root), objects(&root));
    fs::remove_dir_all(other).unwrap();
    fs::remove_dir_all(directory).unwrap();
}

/// A damaged chunk of any level fails only what reads it, naming its level and ordinal, and is
/// neither kept in the cache nor changed: other windows, the overview and the other levels read,
/// and the chunk reads again once it is whole. A damaged overview fails the preparation by name.
#[test]
fn paged_source_damaged_chunks_fail_only_their_readers_by_name() {
    let directory = temp("damaged");
    let root = directory.join("catalog.artifacts");
    let (published, values) = small(&root, &LiveSources::default());
    let reference = pyramid(WIDTH, HEIGHT, values);
    let id = &published.id;
    // The stored chunk `ordinal` of `level`, as the catalog lists every level's in turn.
    let path = |level: usize, ordinal: usize| {
        let before: usize = LEVEL_CHUNKS[..level].iter().sum();
        source_object_path(&root, &published.chunks[before + ordinal].sha256)
    };
    let source = open(&root, &published);
    let refused = |level: usize, rect: [u32; 4], kind: ErrorKind, detail: &str| {
        let error = source
            .window(
                WindowRequest {
                    level,
                    rect,
                    halo: 0,
                },
                &never(),
            )
            .unwrap_err();
        assert_eq!(error.kind, kind, "{error}");
        assert_eq!(error.detail, format!("derived source {id} {detail}"));
    };

    // A flipped bit in level 1's third chunk.
    let original = fs::read(path(1, 2)).unwrap();
    let mut flipped = original.clone();
    flipped[100] ^= 0x20;
    fs::write(path(1, 2), &flipped).unwrap();
    refused(
        1,
        [2048, 0, 1024, 3],
        ErrorKind::SourceUnavailable,
        "level 1 chunk 2 is corrupt",
    );
    refused(
        1,
        [2000, 1, 100, 1],
        ErrorKind::SourceUnavailable,
        "level 1 chunk 2 is corrupt",
    );
    assert!(same_bits(
        &window(&source, 1, [0, 0, 2048, 3], 0).planes,
        &cut(&reference[1], [0, 0, 2048, 3])
    ));
    assert!(same_bits(&whole(&source, 0).planes, &reference[0].planes));
    assert!(same_bits(&whole(&source, 2).planes, &reference[2].planes));
    let error = source.resident_level(&never()).unwrap_err();
    assert!(
        error.detail.contains("level 1 chunk 2 is corrupt"),
        "{error}"
    );
    assert_eq!(fs::read(path(1, 2)).unwrap(), flipped, "left as it is");
    fs::write(path(1, 2), &original).unwrap();
    assert!(same_bits(&whole(&source, 1).planes, &reference[1].planes));

    // A short and a missing level-0 chunk; the cache keeps what it read before.
    let original = fs::read(path(0, 5)).unwrap();
    let cached = source.cached_bytes().0;
    fs::write(path(0, 5), &original[..original.len() - 4]).unwrap();
    let fresh = open(&root, &published);
    let error = fresh
        .window(
            WindowRequest {
                level: 0,
                rect: [5 * 1024, 0, 1, 1],
                halo: 0,
            },
            &never(),
        )
        .unwrap_err();
    assert_eq!(
        error.detail,
        format!("derived source {id} level 0 chunk 5 is corrupt")
    );
    assert!(
        same_bits(
            &window(&source, 0, [5 * 1024, 0, 1, 1], 0).planes,
            &cut(&reference[0], [5 * 1024, 0, 1, 1])
        ),
        "a chunk read whole before is answered from the cache"
    );
    assert_eq!(source.cached_bytes().0, cached);
    fs::remove_file(path(0, 5)).unwrap();
    let error = fresh
        .window(
            WindowRequest {
                level: 0,
                rect: [5 * 1024 + 3, 2, 4, 2],
                halo: 0,
            },
            &never(),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        error.detail,
        format!("derived source {id} level 0 chunk 5 is missing")
    );
    fs::write(path(0, 5), &original).unwrap();

    // A damaged overview fails the preparation by name, and only it.
    let original = fs::read(path(3, 0)).unwrap();
    let mut flipped = original.clone();
    flipped[0] ^= 1;
    fs::write(path(3, 0), &flipped).unwrap();
    let error = prepare_derived(&reading(&root, &published), &never()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        error.detail,
        format!("derived source {id} level 3 chunk 0 is corrupt")
    );
    fs::write(path(3, 0), &original).unwrap();
    open(&root, &published);
    fs::remove_dir_all(directory).unwrap();
}

/// A failure at each write boundary, at every level, is an error that leaves nothing staged and
/// no manifest; a retry publishes the same source.
#[test]
#[slow]
fn paged_source_write_failures_at_each_level_publish_nothing_and_a_retry_completes_it() {
    let values = planes(WIDTH, HEIGHT);
    let reference = {
        let directory = temp("boundaries-reference");
        let root = directory.join("catalog.artifacts");
        let (published, _) = small(&root, &LiveSources::default());
        fs::remove_dir_all(directory).unwrap();
        published
    };
    let first = |level: usize| LEVEL_CHUNKS[..level].iter().sum::<usize>();
    for step in [
        Step::StageChunk(0),
        Step::RenameChunk(16),
        Step::StageChunk(first(1)),
        Step::RenameChunk(first(1) + 8),
        Step::StageChunk(first(2) + 2),
        Step::RenameChunk(first(3)),
        Step::StageChunk(first(3) + 2),
        Step::StageManifest,
        Step::RenameManifest,
        Step::SyncDirectory,
    ] {
        let directory = temp("boundaries");
        let root = directory.join("catalog.artifacts");
        fail::at(step);
        let error = writer(&root, &LiveSources::default())
            .write_planes(WIDTH, HEIGHT, &values, None, &never())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::FileAccess, "{step:?}: {error}");
        assert_eq!(staged(&root), 0, "{step:?} leaves nothing staged");
        assert_eq!(
            objects(&root).contains(reference.id.sha256()),
            step == Step::SyncDirectory,
            "{step:?}: only an unsynced rename leaves the manifest"
        );
        let retried = writer(&root, &LiveSources::default())
            .write_planes(WIDTH, HEIGHT, &values, None, &never())
            .unwrap();
        assert_eq!(retried, reference, "{step:?}");
        let source = open(&root, &retried);
        assert!(same_bits(&whole(&source, 0).planes, &values));
        fs::remove_dir_all(directory).unwrap();
    }
}

/// A chunk outside the grid, one put twice, of another length or with a non-finite sample, a
/// missing chunk at the finish, resident dimensions, a crop outside its reference and a
/// cancellation are refused and publish no manifest.
#[test]
fn paged_source_write_refusals_publish_no_manifest() {
    let values = planes(WIDTH, HEIGHT);
    let directory = temp("refusals");
    let root = directory.join("catalog.artifacts");
    let live = LiveSources::default();
    let writer = writer(&root, &live);
    assert_eq!(
        writer
            .begin_paged(16_384, 7_812, None, &never())
            .err()
            .unwrap()
            .kind,
        ErrorKind::Validation,
        "a resident source is written whole"
    );
    assert_eq!(
        writer
            .begin_paged(65_001, 1, None, &never())
            .err()
            .unwrap()
            .kind,
        ErrorKind::ResourceLimit
    );
    let outside = ReferenceCrop {
        reference_width: WIDTH,
        reference_height: HEIGHT,
        x: 1,
        y: 0,
    };
    assert_eq!(
        writer
            .begin_paged(WIDTH, HEIGHT, Some(outside), &never())
            .err()
            .unwrap()
            .kind,
        ErrorKind::Validation
    );
    let mut write = writer.begin_paged(WIDTH, HEIGHT, None, &never()).unwrap();
    let grid = write.grid().to_vec();
    let chunk = |ordinal: usize| {
        let rect = grid[ordinal];
        cut(
            &Level {
                width: WIDTH,
                height: HEIGHT,
                planes: values.clone(),
            },
            [rect.x, rect.y, rect.width, rect.height],
        )
    };
    let kind = |result: Result<(), Error>| result.unwrap_err().kind;
    assert_eq!(
        kind(write.put(17, &chunk(0), &never())),
        ErrorKind::Validation
    );
    assert_eq!(
        kind(write.put(0, &chunk(0)[1..], &never())),
        ErrorKind::Validation
    );
    let mut bad = chunk(3);
    bad[1000] = f32::INFINITY;
    assert_eq!(kind(write.put(3, &bad, &never())), ErrorKind::Validation);
    write.put(0, &chunk(0), &never()).unwrap();
    assert_eq!(
        kind(write.put(0, &chunk(0), &never())),
        ErrorKind::Validation,
        "a chunk is put once"
    );
    assert_eq!(
        kind(write.put(1, &chunk(1), &AtomicBool::new(true))),
        ErrorKind::Cancelled
    );
    for ordinal in 1..16 {
        write.put(ordinal, &chunk(ordinal), &never()).unwrap();
    }
    let before = objects(&root);
    let error = write.finish(&never()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains("missing 1"), "{error}");
    assert_eq!(objects(&root), before, "nothing built, no manifest");
    // A cancellation while the levels are built leaves no manifest.
    let mut write = writer.begin_paged(WIDTH, HEIGHT, None, &never()).unwrap();
    for ordinal in 0..17 {
        write.put(ordinal, &chunk(ordinal), &never()).unwrap();
    }
    let chunks = objects(&root);
    let error = write.finish(&AtomicBool::new(true)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(objects(&root), chunks, "no level built, no manifest");
    assert_eq!(staged(&root), 0);
    fs::remove_dir_all(directory).unwrap();
}

/// A format 2 manifest of another shape is `incompatible` by name and kept; so is a format 2
/// manifest for resident dimensions, and a format 1 manifest for paged ones.
#[test]
fn paged_source_manifests_of_another_shape_are_refused_and_kept() {
    let directory = temp("manifests");
    let root = directory.join("catalog.artifacts");
    let (published, _) = small(&root, &LiveSources::default());
    let good: serde_json::Value = serde_json::from_slice(
        &fs::read(source_object_path(&root, published.id.sha256())).unwrap(),
    )
    .unwrap();
    let changed = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut value = good.clone();
        change(&mut value);
        value
    };
    let resident = {
        let resident = writer(&root, &LiveSources::default())
            .write_planes(1100, 4, &planes(1100, 4), None, &never())
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(source_object_path(&root, resident.id.sha256())).unwrap(),
        )
        .unwrap()
    };
    let cases: Vec<(serde_json::Value, &str)> = vec![
        (
            changed(&|m| m["format"] = 3.into()),
            "format 3 is not supported",
        ),
        (
            changed(&|m| m["reduction"] = "nearest".into()),
            "is not a source manifest",
        ),
        (
            changed(&|m| {
                m.as_object_mut().unwrap().remove("reduction");
            }),
            "is not a source manifest",
        ),
        (
            changed(&|m| m["chunks"] = serde_json::json!([])),
            "is not a source manifest",
        ),
        (
            changed(&|m| m["levels"][1]["extra"] = true.into()),
            "is not a source manifest",
        ),
        (
            changed(&|m| {
                m["levels"].as_array_mut().unwrap().pop();
            }),
            "lists 3 levels where its dimensions need 4",
        ),
        (
            changed(&|m| {
                let last = m["levels"][3].clone();
                m["levels"].as_array_mut().unwrap().push(last);
            }),
            "lists 5 levels where its dimensions need 4",
        ),
        (
            changed(&|m| m["levels"][2]["width"] = 4097.into()),
            "level 2 is 4097 × 2 where its dimensions need 4098 × 2",
        ),
        (
            changed(&|m| {
                m["levels"][1]["chunks"].as_array_mut().unwrap().pop();
            }),
            "level 1 lists 8 chunks where its dimensions need 9",
        ),
        (
            changed(&|m| m["levels"][2]["chunks"][4]["x"] = 0.into()),
            "level 2 chunk 4 does not match its place in the grid",
        ),
        (
            changed(&|m| m["levels"][3]["chunks"][0]["sha256"] = "../../catalog.sqlite".into()),
            "level 3 chunk 0 does not name a SHA-256",
        ),
        (
            changed(&|m| m["width"] = 16_384.into()),
            "is format 2 for 16384 × 5, within the resident limits",
        ),
        (
            changed(&|m| m["width"] = 65_001.into()),
            "dimensions are out of bounds",
        ),
        (
            changed(&|m| m["chunk_side"] = 2048.into()),
            "uses 2048 pixel chunks",
        ),
        (
            changed(&|m| {
                m["crop"] = serde_json::json!({
                    "reference_width": WIDTH, "reference_height": HEIGHT, "x": 0, "y": 1
                })
            }),
            "crop must lie within its reference frame",
        ),
        (
            {
                let mut value = resident.clone();
                value["width"] = WIDTH.into();
                value
            },
            "dimensions are out of bounds",
        ),
    ];
    for (manifest, reason) in cases {
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        fs::write(source_object_path(&root, &sha256), &bytes).unwrap();
        let read = DerivedSourceRead {
            root: root.clone(),
            catalog_id: "catalog-a".into(),
            id: DerivedSourceId::for_hash(&sha256).unwrap(),
            manifest_bytes: bytes.len() as u64,
        };
        let error = prepare_derived(&read, &never()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible, "{reason}: {error}");
        assert!(error.detail.contains(reason), "{reason}: {error}");
        assert!(error.detail.contains("kept unchanged"), "{error}");
        assert_eq!(fs::read(source_object_path(&root, &sha256)).unwrap(), bytes);
    }
    fs::remove_dir_all(directory).unwrap();
}

/// A collection keeps every level's chunks of a source it retains, and removes every level's of
/// one it does not, with its manifest.
#[test]
fn paged_source_collection_keeps_or_removes_every_level() {
    let directory = temp("collect");
    let root = directory.join("catalog.artifacts");
    let (kept, _) = small(&root, &LiveSources::default());
    let orphan = writer(&root, &LiveSources::default())
        .write_planes(16_385, 1, &vec![0.25; 3 * 16_385], None, &never())
        .unwrap();
    let keep: HashSet<String> = std::iter::once(kept.id.sha256().to_owned())
        .chain(kept.chunks.iter().map(|chunk| chunk.sha256.clone()))
        .collect();
    let orphan_objects: HashSet<String> = std::iter::once(orphan.id.sha256().to_owned())
        .chain(orphan.chunks.iter().map(|chunk| chunk.sha256.clone()))
        .filter(|name| !keep.contains(name))
        .collect();
    assert_eq!(objects(&root).len(), keep.len() + orphan_objects.len());
    let removed = collect_source_objects(&root, &keep, &LiveSources::default(), &never()).unwrap();
    assert_eq!(removed, orphan_objects.len());
    assert_eq!(
        objects(&root),
        keep,
        "every level's chunks of the kept source"
    );
    let source = open(&root, &kept);
    for level in 0..4 {
        whole(&source, level);
    }
    fs::remove_dir_all(directory).unwrap();
}

/// The chunk cache keeps the most recently used chunks within its budget, answers a repeated read
/// without reading, and is charged to the plane gate beside a resident development: the gate
/// counts the development's planes, the overview's and every chunk kept, a chunk the gate would not
/// admit is read through and not kept, the resident level is refused while the gate would not
/// admit it even with the cache emptied, and dropping the source releases everything it held.
#[test]
fn paged_source_cache_is_bounded_and_charged_to_the_plane_gate() {
    let directory = temp("gate");
    let root = directory.join("catalog.artifacts");
    let (published, values) = small(&root, &LiveSources::default());
    let gate = Arc::new(PlaneGate::default());
    let mut development = LinearImage::new(64, 64, vec![0.5; 3 * 64 * 64]).unwrap();
    development.hold(gate.lease());
    let held = 64 * 64 * 12;
    assert_eq!((gate.counted(), gate.counted_bytes()), (1, held));

    let mut source = open(&root, &published);
    assert_eq!(lock(&source.cache).budget, CHUNK_CACHE_BYTES);
    source.hold(&gate);
    let overview = 2_049 * 12;
    assert_eq!(
        gate.counted(),
        3,
        "the development, the overview and the cache"
    );
    assert_eq!(gate.counted_bytes(), held + overview);
    let chunk = 1024 * 5 * 12;
    source.limit_cache(2 * chunk);
    // Five level-0 chunks in turn: the cache keeps the last two, and the gate counts them.
    for column in 0..5 {
        let read = window(&source, 0, [column * 1024, 0, 1024, 5], 0);
        assert!(same_bits(
            &read.planes,
            &cut(
                &pyramid(WIDTH, HEIGHT, values.clone())[0],
                [column * 1024, 0, 1024, 5]
            )
        ));
        let (cached, peak) = source.cached_bytes();
        assert!(cached <= 2 * chunk && peak <= 2 * chunk);
        assert_eq!(gate.counted_bytes(), held + overview + cached);
    }
    assert_eq!(source.cached_bytes(), (2 * chunk, 2 * chunk));
    payload_work::take();
    window(&source, 0, [4 * 1024, 1, 20, 2], 0);
    assert_eq!(payload_work::take(), 0, "the most recent chunk is kept");
    window(&source, 0, [0, 0, 20, 2], 0);
    assert_eq!(payload_work::take(), 1, "the first was evicted");

    // Fill the gate: a chunk it would not admit is read through, and the cache only ever gives
    // up its own chunks to keep another.
    let squeeze = gate.lease();
    squeeze.charge(GATE_ADMITTED_BYTES - gate.counted_bytes());
    let full = gate.counted_bytes();
    let kept = source.cached_bytes().0;
    for column in 5..10 {
        window(&source, 0, [column * 1024, 0, 1024, 5], 0);
        assert!(gate.counted_bytes() <= GATE_ADMITTED_BYTES);
        assert_eq!(gate.counted_bytes(), full);
        assert_eq!(source.cached_bytes().0, kept);
    }
    // The resident level does not fit beside the squeeze even with the cache emptied.
    let error = source.resident_level(&never()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{error}");
    assert_eq!(source.cached_bytes().0, 0, "the cache was emptied first");
    assert_eq!(gate.counted_bytes(), full - kept);
    // With the gate full and the cache empty, a chunk is read through and not kept.
    let squeeze_more = gate.lease();
    squeeze_more.charge(GATE_ADMITTED_BYTES - gate.counted_bytes());
    let read = window(&source, 0, [0, 0, 1024, 5], 0);
    assert!(same_bits(
        &read.planes,
        &cut(&pyramid(WIDTH, HEIGHT, values)[0], [0, 0, 1024, 5])
    ));
    assert_eq!(source.cached_bytes().0, 0);
    assert_eq!(gate.counted_bytes(), GATE_ADMITTED_BYTES);
    drop((squeeze, squeeze_more));
    let resident = source.resident_level(&never()).unwrap();
    let level1 = 8_195 * 3 * 12;
    assert_eq!(gate.counted_bytes(), held + overview + level1);
    assert_eq!(gate.counted(), 4);
    // Dropping the source and its resident level releases everything but the development.
    drop(resident);
    drop(source);
    assert_eq!((gate.counted(), gate.counted_bytes()), (1, held));
    drop(development);
    assert_eq!((gate.counted(), gate.counted_bytes()), (0, 0));
    fs::remove_dir_all(directory).unwrap();
}

/// Fill `chunk`'s planes at `rect` from `value(channel, x, y)`.
fn generate(rect: ChunkRect, chunk: &mut [f32], value: impl Fn(usize, u32, u32) -> f32) {
    let pixels = rect.pixels();
    for channel in 0..3 {
        for row in 0..rect.height {
            for column in 0..rect.width {
                chunk[channel * pixels + (row * rect.width + column) as usize] =
                    value(channel, rect.x + column, rect.y + row);
            }
        }
    }
}

/// Whether `window` holds exactly `value(channel, x, y)` at each of its pixels, in level-0 pixels.
fn holds(window: &PagedWindow, value: impl Fn(usize, u32, u32) -> f32) -> bool {
    let [x, y, width, height] = window.rect;
    let plane = width as usize * height as usize;
    (0..3).all(|channel| {
        (0..height).all(|row| {
            (0..width).all(|column| {
                window.planes[channel * plane + (row * width + column) as usize].to_bits()
                    == value(channel, x + column, y + row).to_bits()
            })
        })
    })
}

/// Each chunk of every further level holds exactly the area averages of the window of the level
/// before it covers, reduced independently; so, with level 0 exact, every level is.
fn every_level_averages_the_one_before(source: &PagedSource) {
    let levels = source.levels();
    for level in 1..levels.len() {
        let (before_w, before_h) = levels[level - 1];
        for rect in grid_of(levels[level].0, levels[level].1) {
            let read = window(source, level, [rect.x, rect.y, rect.width, rect.height], 0);
            let covered = [
                2 * rect.x,
                2 * rect.y,
                (2 * rect.width).min(before_w - 2 * rect.x),
                (2 * rect.height).min(before_h - 2 * rect.y),
            ];
            let before = window(source, level - 1, covered, 0);
            let expected = reduce(&Level {
                width: covered[2],
                height: covered[3],
                planes: before.planes,
            });
            assert_eq!((expected.width, expected.height), (rect.width, rect.height));
            assert!(
                same_bits(&read.planes, &expected.planes),
                "level {level} chunk at {},{}",
                rect.x,
                rect.y
            );
        }
    }
}

/// A paged source two chunk rows high, every chunk different: written chunk by chunk within the
/// writer's scratch bound, level 0 read back bit for bit chunk by chunk and across chunk corners
/// with halos, and every level the exact reduction of the whole level before. Not a measurement.
#[test]
#[slow]
fn paged_source_two_chunk_rows_round_trip_exactly() {
    let directory = temp("rows");
    let root = directory.join("catalog.artifacts");
    let (width, height) = (16_500_u32, 1_030_u32);
    scratch::take_peak();
    let published = writer(&root, &LiveSources::default())
        .write_with(width, height, None, &never(), |rect, chunk| {
            generate(rect, chunk, sample);
            Ok(())
        })
        .unwrap();
    assert!(scratch::take_peak() <= WRITER_SCRATCH_BYTES);
    assert_eq!(published.chunks.len(), 17 * 2 + 9 + 5 + 3);
    payload_work::take();
    let source = open(&root, &published);
    assert_eq!(source.levels(), level_shapes(width, height));
    assert_eq!(payload_work::take(), 1 + 3, "the manifest and the overview");
    for rect in grid_of(width, height) {
        let read = window(&source, 0, [rect.x, rect.y, rect.width, rect.height], 0);
        assert!(
            holds(&read, sample),
            "level 0 chunk at {},{}",
            rect.x,
            rect.y
        );
    }
    for (rect, halo) in [
        ([1000, 1000, 100, 30], 30),
        ([16_000, 1_020, 500, 10], 512),
        ([0, 0, 16_500, 1_030], 0),
    ] {
        let read = window(&source, 0, rect, halo);
        assert!(holds(&read, sample), "{rect:?}");
    }
    every_level_averages_the_one_before(&source);
    let overview = whole(&source, source.overview_level());
    assert!(same_bits(&overview.planes, source.overview().planes()));
    fs::remove_dir_all(directory).unwrap();
}

/// A 512 MP source 65,000 pixels wide, signed values with headroom, negative zeros and
/// subnormals, written as a producer emits it within the writer's scratch bound and prepared from
/// its manifest and overview alone; level 0 reads back bit for bit, chunk by chunk and through
/// halo windows at its far edges, every level is the exact reduction of the one before, the chunk
/// cache stays within its bound, and the resident level is the level within the camera limits.
/// The values repeat every 1,024 pixels, so equal chunks share their objects and the test stores
/// a few dozen; every chunk is still written, hashed, read and verified. Not a measurement.
#[test]
#[slow]
fn paged_source_512_megapixels_65000_wide_round_trips_in_bounded_memory() {
    let directory = temp("large");
    let root = directory.join("catalog.artifacts");
    let (width, height) = (65_000_u32, 7_876_u32);
    assert!(u64::from(width) * u64::from(height) > 511_000_000);
    let value = |channel: usize, x: u32, y: u32| sample(channel, x % 1024, y % 1024);
    scratch::take_peak();
    let published = writer(&root, &LiveSources::default())
        .write_with(width, height, None, &never(), |rect, chunk| {
            generate(rect, chunk, value);
            Ok(())
        })
        .unwrap();
    assert!(scratch::take_peak() <= WRITER_SCRATCH_BYTES);
    published.validate().unwrap();
    assert_eq!(published.chunks.len(), 512 + 128 + 32 + 8 + 4);
    assert!(published.manifest_bytes <= MAX_SOURCE_MANIFEST_BYTES);
    assert_eq!(
        published.payload_bytes(),
        level_shapes(width, height)
            .iter()
            .map(|&(w, h)| u64::from(w) * u64::from(h) * 12)
            .sum::<u64>()
    );

    payload_work::take();
    let source = open(&root, &published);
    assert_eq!(
        payload_work::take(),
        1 + 4,
        "the manifest and the overview's four chunks"
    );
    let overview = source.overview();
    assert_eq!((overview.width(), overview.height()), (4_063, 493));
    assert_eq!(overview.plane_bytes(), 4_063 * 493 * 12);

    let budget = 256 * 1024 * 1024;
    source.limit_cache(budget);
    for rect in grid_of(width, height) {
        let read = window(&source, 0, [rect.x, rect.y, rect.width, rect.height], 0);
        assert!(
            holds(&read, value),
            "level 0 chunk at {},{}",
            rect.x,
            rect.y
        );
    }
    for (rect, halo) in [
        ([64_000, 7_000, 1_000, 876], 64),
        ([30_000, 3_000, 2_100, 1_100], 512),
        ([0, 0, 3, 3], 512),
    ] {
        let read = window(&source, 0, rect, halo);
        assert!(holds(&read, value), "{rect:?}");
    }
    every_level_averages_the_one_before(&source);
    assert!(source.cached_bytes().1 <= budget);
    assert!(same_bits(
        &whole(&source, source.overview_level()).planes,
        overview.planes()
    ));

    // The resident level: the largest within the camera limits, 16,250 × 1,969.
    source.limit_cache(0);
    assert_eq!(source.resident_level_index(), 2);
    let resident = source.resident_level(&never()).unwrap();
    assert_eq!((resident.width(), resident.height()), (16_250, 1_969));
    let plane = 16_250 * 1_969;
    for rect in grid_of(16_250, 1_969) {
        let read = window(&source, 2, [rect.x, rect.y, rect.width, rect.height], 0);
        let rows = rect.height as usize;
        let columns = rect.width as usize;
        let matches = (0..3).all(|channel| {
            (0..rows).all(|row| {
                let at = channel * plane + (rect.y as usize + row) * 16_250 + rect.x as usize;
                same_bits(
                    &read.planes[(channel * rows + row) * columns..][..columns],
                    &resident.planes()[at..at + columns],
                )
            })
        });
        assert!(matches, "resident level at {},{}", rect.x, rect.y);
    }
    drop((resident, source));
    fs::remove_dir_all(directory).unwrap();
}
