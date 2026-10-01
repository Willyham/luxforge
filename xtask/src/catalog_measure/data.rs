//! What the measurements run over, all of it made under the run's own `scratch/` directory: the
//! owner's archive is never read, and the RAW corpus's files are only ever copied from.
//!
//! - **Generated data** (`generate-catalog --files N --assets M --images K`, seed 1): an index of
//!   N files and a catalog of M photographs on a fictional disk, and K real JPEGs with EXIF and
//!   embedded thumbnails.
//! - **A tree** of [`Sizes::tree`] files in folders of [`TREE_FOLDER`], and **a folder** of
//!   [`Sizes::folder`] files, each file a copy of one of the generated JPEGs in turn. A copy is
//!   made with `std::fs::copy`, which on macOS clones the file (APFS copy-on-write: no data blocks
//!   are written) and elsewhere copies it, so every file has its own file identity, as a
//!   photographer's folder does.
//! - **A tree of hard links** ([`Sizes::links`] of them, in folders of [`TREE_FOLDER`]) to the
//!   generated JPEGs in turn, so every JPEG's links (400 at the design's scale) share its file
//!   identity, which the index looks up for every new file to tell a moved file from a new one.
//!   Skipped, with the reason, where the file system refuses hard links.
//! - **A RAW trip** of [`Sizes::trip`] frames, when the corpus is present: the corpus's RAW files
//!   in name order, [`BURST`] consecutive frames from each in turn, copied as above under a
//!   camera's names (`DSC_0001.NEF`, keeping each source's extension), and each copy's EXIF dates
//!   and times rewritten in place so the frames are spread like a trip ([`capture_time`]). The
//!   corpus's own files are never opened for writing; their lengths and modification times are
//!   checked unchanged at the end of the run.
use crate::{generate_catalog, *};
use std::io::{Read, Seek, SeekFrom, Write};

/// How much of each kind of data a scale makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    /// Frames in the RAW trip.
    pub trip: usize,
    /// Files in the tree indexed for the first time.
    pub tree: usize,
    /// Hard links in the tree of links indexed for the first time.
    pub links: usize,
    /// Files in the folder the desktop probes browse.
    pub folder: usize,
    /// Files in the generated index.
    pub files: u32,
    /// Photographs in the generated catalog.
    pub assets: u32,
    /// Real generated JPEGs the tree and the folder are copies of.
    pub images: u32,
    /// RAW picks developed.
    pub picks: usize,
}

/// The design's scale: a 1,000-frame trip, a 200,000-file first index, of copies and of hard
/// links, a 10,000-file folder and index, and a 100,000-photograph catalog.
pub const FULL: Sizes = Sizes {
    trip: 1_000,
    tree: 200_000,
    links: 200_000,
    folder: 10_000,
    files: 10_000,
    assets: 100_000,
    images: 500,
    picks: 20,
};

/// A run that proves the harness end to end in a few minutes and claims nothing.
pub const TINY: Sizes = Sizes {
    trip: 50,
    tree: 2_000,
    links: 2_000,
    folder: 500,
    files: 500,
    assets: 1_000,
    images: 100,
    picks: 5,
};

/// Files in each folder of the tree.
pub const TREE_FOLDER: usize = 500;
/// Consecutive trip frames copied from one corpus file: a burst of one body.
pub const BURST: usize = 4;
/// Bursts a day, one every [`BURST_EVERY_S`] seconds from 08:30.
const BURSTS_A_DAY: usize = 50;
const BURST_EVERY_S: usize = 7 * 60;
/// How far into a RAW file its EXIF dates are looked for: every header the corpus's formats keep,
/// the embedded preview's EXIF included, is within it.
const HEADER_WINDOW: u64 = 512 * 1024;
/// The generated data's seed.
const SEED: u64 = 1;

/// The extensions of the RAW formats a corpus file may have, lowercase.
const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "arw", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq", "kdc", "mef", "mos",
    "mrw", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "x3f",
];

/// A folder of copies, or of hard links.
#[derive(Debug)]
pub struct Tree {
    pub dir: PathBuf,
    pub files: usize,
    pub folders: usize,
}

/// One frame of the trip.
#[derive(Debug)]
pub struct Frame {
    pub path: PathBuf,
    pub source: PathBuf,
    /// Its capture time, as EXIF writes it.
    pub capture: String,
    /// How many EXIF dates and times the copy's header held and had rewritten.
    pub rewritten: usize,
}

/// The RAW trip.
#[derive(Debug)]
pub struct Trip {
    pub dir: PathBuf,
    pub frames: Vec<Frame>,
}

/// A corpus file's length and modification time, which the run must leave as they were.
type Signature = (u64, Option<std::time::SystemTime>);

/// Everything the steps run over.
pub struct DataSet {
    pub scratch: PathBuf,
    /// The generated catalog and index (`catalog.sqlite`).
    pub catalog: PathBuf,
    /// The generated JPEGs' folder.
    pub images: PathBuf,
    pub tree: Tree,
    /// The tree of hard links, or why there is none.
    pub links: std::result::Result<Tree, String>,
    pub folder: Tree,
    /// The trip, or why there is none.
    pub trip: std::result::Result<Trip, String>,
    corpus: Vec<(PathBuf, Signature)>,
    pub record: Value,
}

impl DataSet {
    /// Make everything under `scratch`, which must not exist.
    pub fn prepare(scratch: &Path, sizes: Sizes, corpus_dir: Option<&Path>) -> Result<Self> {
        ensure(!scratch.exists(), "The scratch directory must be new")?;
        fs::create_dir_all(scratch)?;
        let started = std::time::Instant::now();
        let generated = scratch.join("generated");
        generate_catalog::run(
            &generated,
            &generate_catalog::Options {
                seed: SEED,
                files: Some(sizes.files),
                assets: Some(sizes.assets),
                images: Some(sizes.images),
            },
        )?;
        let generated_s = started.elapsed().as_secs_f64();
        let images = generated.join("images");
        let jpegs: Vec<PathBuf> = files(&images)?
            .into_iter()
            .filter(|path| has_extension(path, &["jpg", "jpeg"]))
            .collect();
        ensure(!jpegs.is_empty(), "generate-catalog wrote no images")?;
        let started = std::time::Instant::now();
        let tree = write_tree(&jpegs, &scratch.join("tree"), sizes.tree, Some(TREE_FOLDER))?;
        let folder = write_tree(&jpegs, &scratch.join("folder"), sizes.folder, None)?;
        let copies_s = started.elapsed().as_secs_f64();
        let started = std::time::Instant::now();
        let links = write_links(&jpegs, &scratch.join("links"), sizes.links, TREE_FOLDER)?;
        let links_s = started.elapsed().as_secs_f64();
        let started = std::time::Instant::now();
        let (trip, corpus) = match corpus(corpus_dir) {
            Ok(sources) => {
                let signed = sources
                    .iter()
                    .map(|path| Ok((path.clone(), signature(path)?)))
                    .collect::<Result<Vec<_>>>()?;
                (
                    Ok(write_trip(&sources, &scratch.join("trip"), sizes.trip)?),
                    signed,
                )
            }
            Err(reason) => (Err(reason), Vec::new()),
        };
        let trip_s = started.elapsed().as_secs_f64();
        let record = json!({
            "seed": SEED,
            "sizes": {
                "trip": sizes.trip, "tree": sizes.tree, "folder": sizes.folder,
                "files": sizes.files, "assets": sizes.assets, "images": sizes.images,
                "picks": sizes.picks,
            },
            "generated": {
                "path": generated,
                "command": format!(
                    "cargo xtask generate-catalog --output {} --files {} --assets {} --images {} --seed {SEED}",
                    generated.display(), sizes.files, sizes.assets, sizes.images
                ),
                "images": jpegs.len(),
                "seconds": generated_s,
            },
            "copies": {
                "method": "std::fs::copy of the generated JPEGs in turn: an APFS clone (copy-on-write) on macOS, a full copy elsewhere; copies, not hard links, so every file has its own identity",
                "tree": {"path": tree.dir, "files": tree.files, "folders": tree.folders, "per_folder": TREE_FOLDER},
                "folder": {"path": folder.dir, "files": folder.files},
                "seconds": copies_s,
            },
            "links": match &links {
                Ok(links) => json!({
                    "method": "std::fs::hard_link to the generated JPEGs in turn: every link of one JPEG shares its file identity",
                    "path": links.dir,
                    "links": links.files,
                    "folders": links.folders,
                    "per_folder": TREE_FOLDER,
                    "sources": jpegs.len(),
                    "links_per_source": links.files.div_ceil(jpegs.len()),
                    "seconds": links_s,
                }),
                Err(reason) => json!({"skipped": reason}),
            },
            "trip": match &trip {
                Ok(trip) => json!({
                    "path": trip.dir,
                    "frames": trip.frames.len(),
                    "corpus_files": corpus.len(),
                    "sources_used": trip.frames.iter().map(|frame| &frame.source).collect::<std::collections::HashSet<_>>().len(),
                    "corpus": corpus_dir,
                    "burst": BURST,
                    "first_capture": trip.frames.first().map(|frame| frame.capture.clone()),
                    "last_capture": trip.frames.last().map(|frame| frame.capture.clone()),
                    "frames_with_dates_rewritten": trip.frames.iter().filter(|frame| frame.rewritten > 0).count(),
                    "method": "copies of the corpus's RAW files in name order, BURST consecutive frames from each, named DSC_0001 on with each source's extension, every EXIF date and time in each copy's first 512 KiB rewritten in place to the frame's capture time",
                    "seconds": trip_s,
                }),
                Err(reason) => json!({"skipped": reason}),
            },
        });
        Ok(Self {
            scratch: scratch.into(),
            catalog: generated.join(generate_catalog::CATALOG),
            images,
            tree,
            links,
            folder,
            trip,
            corpus,
            record,
        })
    }

    /// Whether every corpus file still has the length and modification time it had before the
    /// run: the originals are only ever read.
    pub fn corpus_unchanged(&self) -> Result<bool> {
        for (path, before) in &self.corpus {
            if signature(path)? != *before {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn signature(path: &Path) -> Result<Signature> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.len(), metadata.modified().ok()))
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.contains(&extension.to_ascii_lowercase().as_str()))
}

/// The corpus's RAW files in name order, or why there are none: no directory named, one that does
/// not exist, or one that holds no RAW file. Hidden files are left out.
pub fn corpus(dir: Option<&Path>) -> std::result::Result<Vec<PathBuf>, String> {
    let dir = dir.ok_or(
        "corpus absent: no --raw-corpus given and LUXFORGE_RAW_CORPUS_DIR is not set".to_owned(),
    )?;
    if !dir.is_dir() {
        return Err(format!(
            "corpus absent: {} is not a directory",
            dir.display()
        ));
    }
    let mut found: Vec<PathBuf> = files(dir)
        .map_err(|error| format!("corpus unreadable: {}: {error}", dir.display()))?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with('.'))
                && has_extension(path, RAW_EXTENSIONS)
        })
        .collect();
    found.sort_by_key(|path| path.file_name().map(ToOwned::to_owned));
    if found.is_empty() {
        return Err(format!(
            "corpus absent: {} holds no RAW files",
            dir.display()
        ));
    }
    Ok(found)
}

/// `files` copies of `sources` in turn under `dir`, `IMG_000001.<ext>` on, in folders of
/// `per_folder` (`0000/`, `0001/`, …) or all in `dir`.
pub fn write_tree(
    sources: &[PathBuf],
    dir: &Path,
    files: usize,
    per_folder: Option<usize>,
) -> Result<Tree> {
    ensure(!sources.is_empty(), "A tree needs at least one source file")?;
    ensure(per_folder != Some(0), "A folder holds at least one file")?;
    fs::create_dir_all(dir)?;
    let mut folders = usize::from(per_folder.is_none());
    for index in 0..files {
        let folder = match per_folder {
            Some(per) => {
                let folder = dir.join(format!("{:04}", index / per));
                if index % per == 0 {
                    fs::create_dir_all(&folder)?;
                    folders += 1;
                }
                folder
            }
            None => dir.to_path_buf(),
        };
        let source = &sources[index % sources.len()];
        let extension = source
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        fs::copy(
            source,
            folder.join(format!("IMG_{:06}.{extension}", index + 1)),
        )?;
    }
    Ok(Tree {
        dir: dir.into(),
        files,
        folders,
    })
}

/// `links` hard links to `sources` in turn under `dir`, `IMG_000001.<ext>` on, in folders of
/// `per_folder` (`0000/`, `0001/`, …): every link of one source shares its file identity. `Ok(Err)`
/// with the reason when the file system refuses a hard link.
pub fn write_links(
    sources: &[PathBuf],
    dir: &Path,
    links: usize,
    per_folder: usize,
) -> Result<std::result::Result<Tree, String>> {
    ensure(!sources.is_empty(), "A tree needs at least one source file")?;
    ensure(per_folder != 0, "A folder holds at least one file")?;
    fs::create_dir_all(dir)?;
    let mut folders = 0;
    for index in 0..links {
        let folder = dir.join(format!("{:04}", index / per_folder));
        if index % per_folder == 0 {
            fs::create_dir_all(&folder)?;
            folders += 1;
        }
        let source = &sources[index % sources.len()];
        let extension = source
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        let link = folder.join(format!("IMG_{:06}.{extension}", index + 1));
        if let Err(error) = fs::hard_link(source, &link) {
            return Ok(Err(format!(
                "the file system refused a hard link from {} to {}: {error}",
                link.display(),
                source.display()
            )));
        }
    }
    Ok(Ok(Tree {
        dir: dir.into(),
        files: links,
        folders,
    }))
}

/// A trip frame's name: `DSC_0001.<the source's extension>` for the first frame.
pub fn frame_name(index: usize, source: &Path) -> String {
    let extension = source
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("DSC_{:04}.{extension}", index + 1)
}

/// A trip frame's capture time, as EXIF writes it: bursts of [`BURST`] frames a second apart, a
/// burst every [`BURST_EVERY_S`] seconds from 08:30, [`BURSTS_A_DAY`] bursts a day, from the 1st
/// of June 2026.
pub fn capture_time(index: usize) -> String {
    let burst = index / BURST;
    let day = burst / BURSTS_A_DAY;
    let seconds = 8 * 3600 + 30 * 60 + (burst % BURSTS_A_DAY) * BURST_EVERY_S + index % BURST;
    let date = time::Date::from_calendar_date(2026, time::Month::June, 1).expect("a calendar date")
        + time::Duration::days(day as i64);
    format!(
        "{:04}:{:02}:{:02} {:02}:{:02}:{:02}",
        date.year(),
        u8::from(date.month()),
        date.day(),
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The trip: `frames` copies of `corpus` under `dir`, [`BURST`] from each file in turn, each named
/// by [`frame_name`] and its EXIF dates rewritten to [`capture_time`].
pub fn write_trip(corpus: &[PathBuf], dir: &Path, frames: usize) -> Result<Trip> {
    ensure(!corpus.is_empty(), "A trip needs at least one corpus file")?;
    fs::create_dir_all(dir)?;
    let mut written = Vec::with_capacity(frames);
    for index in 0..frames {
        let source = &corpus[(index / BURST) % corpus.len()];
        let path = dir.join(frame_name(index, source));
        fs::copy(source, &path)?;
        // A copy keeps its source's permissions; the copy, never the source, is made writable.
        let mut permissions = fs::metadata(&path)?.permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the copy is the harness's own scratch file"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions)?;
        let capture = capture_time(index);
        let rewritten = rewrite_capture_times(&path, &capture)?;
        written.push(Frame {
            path,
            source: source.clone(),
            capture,
            rewritten,
        });
    }
    Ok(Trip {
        dir: dir.into(),
        frames: written,
    })
}

/// Rewrite every EXIF date and time in the first [`HEADER_WINDOW`] bytes of `path` to `when`,
/// writing only those bytes, and say how many there were.
fn rewrite_capture_times(path: &Path, when: &str) -> Result<usize> {
    ensure(when.len() == 19, "An EXIF date and time is 19 bytes")?;
    let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut head = Vec::new();
    (&mut file).take(HEADER_WINDOW).read_to_end(&mut head)?;
    let found = datetimes(&head);
    for offset in &found {
        file.seek(SeekFrom::Start(*offset as u64))?;
        file.write_all(when.as_bytes())?;
    }
    Ok(found.len())
}

/// The offsets of every EXIF date and time, `YYYY:MM:DD HH:MM:SS` with plausible fields, in
/// `bytes`.
pub fn datetimes(bytes: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 19 <= bytes.len() {
        if is_datetime(&bytes[at..at + 19]) {
            found.push(at);
            at += 19;
        } else {
            at += 1;
        }
    }
    found
}

fn is_datetime(text: &[u8]) -> bool {
    let number = |range: std::ops::Range<usize>| {
        text[range].iter().try_fold(0u32, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(byte - b'0'))
        })
    };
    if text[4] != b':'
        || text[7] != b':'
        || text[10] != b' '
        || text[13] != b':'
        || text[16] != b':'
    {
        return false;
    }
    matches!(
        (
            number(0..4),
            number(5..7),
            number(8..10),
            number(11..13),
            number(14..16),
            number(17..19)
        ),
        (
            Some(1900..=2099),
            Some(1..=12),
            Some(1..=31),
            Some(0..=23),
            Some(0..=59),
            Some(0..=60)
        )
    )
}
