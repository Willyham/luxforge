//! Derived image sources: a photograph's immutable float pixels that the host made, an HDR merge's
//! result, rather than read from an original file (`docs/design/hdr-merge.md#durable-derived-source`).
//!
//! A source is stored under the catalog's artifact root, beside the derived artifacts but never
//! among them, as immutable content-addressed objects in `sources/`, each named by the SHA-256 of
//! its bytes:
//!
//! - **Chunks** of at most 1024 × 1024 pixels, [`CHUNK_SIDE`], tiling the frame row by row: the
//!   chunk's red plane, row by row, then its green and blue planes, each sample an IEEE-754 binary32
//!   in little-endian byte order, uncompressed. A full chunk is 12 MiB; no chunk passes
//!   [`MAX_CHUNK_BYTES`] and no source holds more than [`MAX_CHUNKS`].
//! - One **manifest**, a small JSON document of format [`SOURCE_MANIFEST_FORMAT`] naming the
//!   dimensions, the colour interpretation (linear sRGB primaries, D65 white, linear transfer), the
//!   sample encoding, the chunk layout and side, an optional crop within the reference frame, and
//!   each chunk's rectangle, length and SHA-256. The source's identity is the SHA-256 of the
//!   manifest's bytes, so it names exactly one content.
//!
//! Writing, reading and collecting run here, on a worker. A writer stages each object in the
//! shared `tmp/`, syncs it and renames it into `sources/`, then writes the manifest last and syncs
//! the directory once, so a crash leaves either nothing the catalog can see or a complete source;
//! a reader verifies the manifest, every chunk's coordinates, length and hash and every sample's
//! finiteness while it streams the chunks into one [`LinearImage`] allocation. The catalog owner
//! records what a writer returned ([`PublishedSource`]) and stats objects; it never reads or hashes
//! a payload. The catalog's asset-source and manifest-chunk references retain a source
//! (`crate::editor` `derived_source`), and a collection removes only objects nothing retains and no
//! writer of this process published.
// A derived photograph's preparation reads through here. The HDR merge's publication
// (`docs/design/hdr-merge.md`) writes through here; until it does, only
// the tests and the qualification harness call the writer.
#![cfg_attr(not(test), allow(dead_code))]
use super::{
    lock,
    store::{RootState, TEMPORARY, root_state},
};
use crate::{
    Error,
    atomic_file::{self, file_error},
    source::{LinearImage, layout},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// The directory under the artifact root that holds every source object, manifests and chunks.
pub(crate) const SOURCES: &str = "sources";
/// The side of a full chunk, in pixels.
pub(crate) const CHUNK_SIDE: u32 = 1024;
/// The most chunks one source may hold: a 16,384-pixel square at [`CHUNK_SIDE`].
pub(crate) const MAX_CHUNKS: usize = 256;
/// The largest chunk object: 16 MiB, above the 12 MiB of a full RGB chunk.
pub(crate) const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;
/// The current manifest shape. Any other marker is refused by name and left as it is.
pub(crate) const SOURCE_MANIFEST_FORMAT: u32 = 1;
/// The largest manifest: 256 chunk entries are about 36 KiB.
pub(crate) const MAX_SOURCE_MANIFEST_BYTES: u64 = 64 * 1024;
const PREFIX: &str = "source-";
/// Bytes per stored sample: one little-endian binary32.
const SAMPLE_BYTES: u64 = 4;

/// Serializes a writer's decision that an object is live with a collection's decision to remove
/// it, as `OBJECT_DECISIONS` does for artifacts: a writer marks each object live under this lock
/// before it relies on the file, and a collection checks liveness and removes under it, so no
/// collection deletes an object a publication is about to hand to the catalog. Only workers take
/// it.
static SOURCE_DECISIONS: Mutex<()> = Mutex::new(());

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    #[cfg(test)]
    payload_work::hashed();
    format!("{:x}", Sha256::digest(bytes))
}

/// One derived source's identity: `source-` and the lowercase hexadecimal SHA-256 of its
/// manifest's bytes, which name every chunk's hash, so it names exactly one content. Clients treat
/// it as opaque.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DerivedSourceId(String);

impl DerivedSourceId {
    /// The identity of the manifest whose SHA-256 is `sha256`.
    pub(crate) fn for_hash(sha256: &str) -> Result<Self, Error> {
        Self::parse(format!("{PREFIX}{sha256}"))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.strip_prefix(PREFIX).is_some_and(is_sha256) {
            Ok(Self(value))
        } else {
            Err(Error::validation("invalid DerivedSourceId"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The SHA-256 of the manifest, which is also its object's name.
    pub(crate) fn sha256(&self) -> &str {
        &self.0[PREFIX.len()..]
    }
}

impl std::fmt::Display for DerivedSourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for DerivedSourceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DerivedSourceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// A derived source's colour: linear sRGB primaries, D65 white and linear transfer, the space every
/// prepared source develops into. Values may be negative or above one; only non-finite values are
/// refused. The one interpretation so far; another is a new manifest shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivedColour {
    LinearSrgbD65,
}

/// How each stored sample is encoded: IEEE-754 binary32, little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SampleEncoding {
    F32Le,
}

/// How a chunk orders its samples: its whole red plane row by row, then green, then blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ChunkLayout {
    PlanarRgb,
}

/// A photograph's derived source as its `SourceKind::Derived` interpretation records it: which
/// stored source it is, its dimensions and its colour. Never a sensor interpretation: a derived
/// source has no mosaic, white balance or camera calibration of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedInterpretation {
    pub source_id: DerivedSourceId,
    pub width: u32,
    pub height: u32,
    pub colour: DerivedColour,
}

/// Where a derived source lies in the upright frame of the reference it was made against: the
/// reference's dimensions and the source's top-left corner there. The source's own dimensions are
/// the crop's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCrop {
    pub reference_width: u32,
    pub reference_height: u32,
    pub x: u32,
    pub y: u32,
}

/// The refusal of a path that reads an original file — a camera preview, a 100% region of the
/// original — for a derived photograph, which has none: its pixels are its stored source's, which
/// preparation, rendering and its rendered previews read. Named, never a fallback to another kind's
/// reading.
pub(crate) fn no_original_file(what: &str) -> Error {
    Error::unsupported_input(format!(
        "{what} is not available for a derived photograph, which has no original file"
    ))
}

/// One chunk's rectangle in the source's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChunkRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl ChunkRect {
    fn pixels(self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The chunk's stored length: three planes of binary32 samples.
    fn bytes(self) -> u64 {
        self.pixels() as u64 * 3 * SAMPLE_BYTES
    }
}

/// The chunks a `width` × `height` source is stored in, row by row and left to right, after the
/// dimensions pass the linear-source bound (16,384 pixels a side, 128 megapixels, 1.5 GiB).
pub(crate) fn chunk_grid(width: u32, height: u32) -> Result<Vec<ChunkRect>, Error> {
    layout(width, height)?;
    let mut grid = Vec::new();
    for y in (0..height).step_by(CHUNK_SIDE as usize) {
        for x in (0..width).step_by(CHUNK_SIDE as usize) {
            grid.push(ChunkRect {
                x,
                y,
                width: CHUNK_SIDE.min(width - x),
                height: CHUNK_SIDE.min(height - y),
            });
        }
    }
    debug_assert!(grid.len() <= MAX_CHUNKS && grid.iter().all(|c| c.bytes() <= MAX_CHUNK_BYTES));
    Ok(grid)
}

/// The manifest as stored, its marker first. `deny_unknown_fields` and the closed enumerations make
/// every other shape `incompatible`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceManifest {
    format: u32,
    width: u32,
    height: u32,
    colour: DerivedColour,
    sample: SampleEncoding,
    layout: ChunkLayout,
    chunk_side: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crop: Option<ReferenceCrop>,
    chunks: Vec<ChunkEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkEntry {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    bytes: u64,
    sha256: String,
}

impl SourceManifest {
    /// Everything the shape cannot say: the dimensions within the source bound, the chunk side,
    /// the crop within its reference, and the chunks exactly the grid, each with its length and a
    /// well-formed hash. A hash is a name under `sources/` and nothing else, so no entry can reach
    /// outside it.
    fn check(&self, id: &DerivedSourceId) -> Result<Vec<ChunkRect>, Error> {
        let incompatible = |reason: String| {
            Error::incompatible(format!(
                "derived source {id} manifest {reason}; the source is kept unchanged"
            ))
        };
        if self.chunk_side != CHUNK_SIDE {
            return Err(incompatible(format!(
                "uses {} pixel chunks, not {CHUNK_SIDE}",
                self.chunk_side
            )));
        }
        let grid = chunk_grid(self.width, self.height).map_err(|error| {
            incompatible(format!("dimensions are out of bounds ({})", error.detail))
        })?;
        if let Some(crop) = self.crop {
            check_crop(crop, self.width, self.height)
                .map_err(|error| incompatible(error.detail))?;
        }
        if self.chunks.len() != grid.len() || self.chunks.len() > MAX_CHUNKS {
            return Err(incompatible(format!(
                "lists {} chunks where its dimensions need {}",
                self.chunks.len(),
                grid.len()
            )));
        }
        for (ordinal, (entry, rect)) in self.chunks.iter().zip(&grid).enumerate() {
            let listed = ChunkRect {
                x: entry.x,
                y: entry.y,
                width: entry.width,
                height: entry.height,
            };
            if listed != *rect || entry.bytes != rect.bytes() || entry.bytes > MAX_CHUNK_BYTES {
                return Err(incompatible(format!(
                    "chunk {ordinal} does not match its place in the grid"
                )));
            }
            if !is_sha256(&entry.sha256) {
                return Err(incompatible(format!(
                    "chunk {ordinal} does not name a SHA-256"
                )));
            }
        }
        Ok(grid)
    }
}

fn check_crop(crop: ReferenceCrop, width: u32, height: u32) -> Result<(), Error> {
    let inside = crop
        .x
        .checked_add(width)
        .is_some_and(|right| right <= crop.reference_width)
        && crop
            .y
            .checked_add(height)
            .is_some_and(|bottom| bottom <= crop.reference_height);
    if inside {
        Ok(())
    } else {
        Err(Error::validation(
            "a derived source's crop must lie within its reference frame",
        ))
    }
}

/// One chunk of a published source as the catalog records it, in manifest order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishedChunk {
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

/// A source a writer made durable, which the catalog does not know yet: what the owner records in
/// one transaction with the photograph that holds it (`crate::editor` `link_asset_source`). Every
/// field is the writer's own account of the bytes it wrote and hashed; recording it stats files and
/// reads none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishedSource {
    pub(crate) id: DerivedSourceId,
    pub(crate) manifest_bytes: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) colour: DerivedColour,
    pub(crate) chunks: Vec<PublishedChunk>,
}

impl PublishedSource {
    /// The interpretation a photograph of this source records.
    pub(crate) fn interpretation(&self) -> DerivedInterpretation {
        DerivedInterpretation {
            source_id: self.id.clone(),
            width: self.width,
            height: self.height,
            colour: self.colour,
        }
    }

    /// The pixel bytes the chunks hold together.
    pub(crate) fn payload_bytes(&self) -> u64 {
        self.chunks.iter().map(|chunk| chunk.bytes).sum()
    }

    /// A record is only accepted whole: the dimensions within the source bound, a chunk for each
    /// place in the grid with that place's length and a well-formed hash, and a bounded manifest.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let grid = chunk_grid(self.width, self.height)?;
        let chunks_fit = self.chunks.len() == grid.len()
            && self
                .chunks
                .iter()
                .zip(&grid)
                .all(|(chunk, rect)| chunk.bytes == rect.bytes() && is_sha256(&chunk.sha256));
        if !chunks_fit {
            return Err(Error::validation(format!(
                "derived source {} does not list one chunk for each place in its grid",
                self.id
            )));
        }
        if self.manifest_bytes == 0 || self.manifest_bytes > MAX_SOURCE_MANIFEST_BYTES {
            return Err(Error::resource_limit(format!(
                "derived source {} has a manifest of {} bytes; at most {MAX_SOURCE_MANIFEST_BYTES}",
                self.id, self.manifest_bytes
            )));
        }
        Ok(())
    }
}

/// The source objects published while one editor service is open, by SHA-256: by its writers, from
/// any thread. No collection removes one, because the catalog may be about to record it or already
/// has. Shared by the service, its writers and its collections; reopening the catalog starts an
/// empty set, so what a crash or a failed transaction left unrecorded is collected then.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveSources(Arc<Mutex<HashSet<String>>>);

impl LiveSources {
    fn mark(&self, sha256: &str) {
        lock(&self.0).insert(sha256.to_owned());
    }

    pub(crate) fn contains(&self, sha256: &str) -> bool {
        lock(&self.0).contains(sha256)
    }

    /// Whether every object of `source` was published while the service is open.
    pub(crate) fn holds(&self, source: &DerivedSourceId) -> bool {
        self.contains(source.sha256())
    }
}

/// Where one source object lives under a root.
pub(crate) fn source_object_path(root: &Path, sha256: &str) -> PathBuf {
    root.join(SOURCES).join(sha256)
}

/// The `sources/` directory of `root`, refused when it is anything but a directory of its own: a
/// link would put the objects the catalog verifies somewhere else.
fn sources_dir(root: &Path) -> Result<Option<PathBuf>, Error> {
    let dir = root.join(SOURCES);
    match fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(file_error(
            "cannot read derived source directory",
            error.kind(),
        )),
        Ok(metadata) if metadata.is_dir() => Ok(Some(dir)),
        Ok(_) => Err(Error::incompatible(format!(
            "derived source directory {} is not a directory of the artifact root",
            dir.display()
        ))),
    }
}

/// A source object's length when it is a regular file of its own, `None` when it is absent, and a
/// refusal when the name is a link or anything else, so a read never follows a path out of the
/// root. Stat only: the catalog owner may call it.
pub(crate) fn object_length(root: &Path, sha256: &str) -> Result<Option<u64>, Error> {
    let path = source_object_path(root, sha256);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(file_error(
            "cannot read derived source object",
            error.kind(),
        )),
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err(Error::incompatible(format!(
            "derived source object {} is not a file of the artifact root",
            path.display()
        ))),
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::cancelled("derived source job cancelled"))
    } else {
        Ok(())
    }
}

/// Fill `buffer` from `file`, retrying interrupted reads, and answer whether the file ended
/// exactly there.
fn read_exactly(file: &mut File, buffer: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let mut probe = [0_u8; 1];
    loop {
        match file.read(&mut probe) {
            Ok(read) => return Ok(read == 0),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Whether the object `sha256` already holds exactly `bytes`.
fn object_holds(root: &Path, sha256: &str, bytes: &[u8]) -> Result<bool, Error> {
    if object_length(root, sha256)? != Some(bytes.len() as u64) {
        return Ok(false);
    }
    let mut file = match File::open(source_object_path(root, sha256)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(file_error(
                "cannot read derived source object",
                error.kind(),
            ));
        }
        Ok(file) => file,
    };
    let mut held = vec![0; bytes.len()];
    let whole = read_exactly(&mut file, &mut held)
        .map_err(|error| file_error("cannot read derived source object", error.kind()))?;
    Ok(whole && held == bytes)
}

/// The writer's boundaries, which a test may make fail ([`fail::at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Writing or syncing chunk `n`'s staged file.
    StageChunk(usize),
    /// Renaming chunk `n` into `sources/`.
    RenameChunk(usize),
    /// Writing or syncing the staged manifest.
    StageManifest,
    /// Renaming the manifest into `sources/`.
    RenameManifest,
    /// Syncing `sources/` after the manifest.
    SyncDirectory,
}

/// Whether a test asked the writer to fail at `step`; always passes outside tests.
fn inject(step: Step) -> io::Result<()> {
    #[cfg(test)]
    return fail::check(step);
    #[cfg(not(test))]
    {
        let _ = step;
        Ok(())
    }
}

/// Failure injection for the writer's boundaries, set per thread by a test: the writer fails at the
/// named step as the file system would, cleaning up what it staged, and every step before it stays
/// as it happened.
#[cfg(test)]
pub(crate) mod fail {
    use super::Step;
    use std::cell::Cell;

    thread_local! {
        static AT: Cell<Option<Step>> = const { Cell::new(None) };
    }

    /// Fail the next write on this thread at `step`.
    pub(crate) fn at(step: Step) {
        AT.set(Some(step));
    }

    pub(super) fn check(step: Step) -> std::io::Result<()> {
        if AT.get() == Some(step) {
            AT.set(None);
            return Err(std::io::Error::other(format!(
                "injected failure at {step:?}"
            )));
        }
        Ok(())
    }
}

/// How many payloads this thread hashed, for the tests that prove the catalog owner's requests read
/// and hash none: a writer hashes every chunk and manifest it encodes, a reader every one it
/// verifies.
#[cfg(test)]
pub(crate) mod payload_work {
    use std::cell::Cell;

    thread_local! {
        static WORK: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn hashed() {
        WORK.set(WORK.get() + 1);
    }

    /// The payloads this thread hashed since it last asked.
    pub(crate) fn take() -> u64 {
        WORK.replace(0)
    }
}

/// Planes of a merge's result for the tests, `width × height` as [`LinearImage`] holds them: in
/// each channel a ramp from below zero to four times diffuse white, and every 61st column a patch
/// out of the sRGB gamut whose red is negative.
#[cfg(test)]
pub(crate) fn merged_planes(width: u32, height: u32) -> Vec<f32> {
    let plane = (width * height) as usize;
    let mut values = vec![0.0_f32; 3 * plane];
    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / (width - 1).max(1) as f32;
            let v = y as f32 / (height - 1).max(1) as f32;
            let mut rgb = [
                -0.2 + 4.2 * u * u,
                -0.1 + 3.0 * u * v,
                -0.15 + 1.9 * v * (1.0 - u),
            ];
            if x % 61 < 4 {
                rgb = [-0.3, 0.8 + v, 0.4];
            }
            let at = (y * width + x) as usize;
            for (channel, value) in rgb.into_iter().enumerate() {
                values[channel * plane + at] = value;
            }
        }
    }
    values
}

/// Publishes derived sources into one catalog's artifact root from a worker thread: the worker
/// writes and hashes the chunks and the manifest, and the catalog owner records the returned
/// [`PublishedSource`] in the transaction that gives it to a photograph. Obtained from
/// `EditorService::derived_source_writer`; cloning it is cheap.
#[derive(Clone, Debug)]
pub(crate) struct DerivedSourceWriter {
    artifacts: super::ArtifactWriter,
    root: PathBuf,
    live: LiveSources,
}

impl DerivedSourceWriter {
    pub(crate) fn new(root: PathBuf, catalog_id: String, live: LiveSources) -> Self {
        Self {
            artifacts: super::ArtifactWriter::new(
                root.clone(),
                catalog_id,
                super::LiveArtifacts::default(),
            ),
            root,
            live,
        }
    }

    /// Publish a `width` × `height` source whose chunks `fill` produces, one at a time, and return
    /// its record. `fill` is handed each chunk's rectangle ([`chunk_grid`] order) and a buffer of
    /// its three planes — `width × height` red values row by row, then green, then blue — to write
    /// in full; so a caller that streams its result never holds more than one chunk here.
    ///
    /// The root is claimed on first use as an artifact write claims it, and a root that is another
    /// catalog's is `incompatible`. Each chunk is encoded, refused with `validation` if any sample
    /// is not finite, hashed, staged in `tmp/`, synced and renamed to `sources/<sha256>`, so the
    /// staging area never holds more than one chunk; an object that already holds the same bytes
    /// is kept, and one that holds anything else is replaced. The manifest is written last the
    /// same way, and one sync of `sources/` then makes every rename durable. Every object is live
    /// from before the writer relies on it until the service that made this writer closes, so no
    /// collection removes it before the catalog records it. A failure or cancellation at any step
    /// leaves no manifest, so nothing can name the chunks already written, which a later collection
    /// removes once they are no longer live. Worker work: about one chunk of scratch, 12 MiB of
    /// samples and their encoding.
    pub(crate) fn write_with(
        &self,
        width: u32,
        height: u32,
        crop: Option<ReferenceCrop>,
        cancel: &AtomicBool,
        mut fill: impl FnMut(ChunkRect, &mut [f32]) -> Result<(), Error>,
    ) -> Result<PublishedSource, Error> {
        let grid = chunk_grid(width, height)?;
        if let Some(crop) = crop {
            check_crop(crop, width, height)?;
        }
        cancelled(cancel)?;
        self.artifacts.prepare_root()?;
        let sources = self.root.join(SOURCES);
        fs::create_dir_all(&sources)
            .map_err(|error| file_error("cannot create derived source directory", error.kind()))?;
        sources_dir(&self.root)?;
        let largest = grid.first().map_or(0, |rect| rect.pixels());
        let mut samples = vec![0.0_f32; 3 * largest];
        let mut encoded: Vec<u8> = Vec::with_capacity(3 * largest * SAMPLE_BYTES as usize);
        let mut entries = Vec::with_capacity(grid.len());
        for (ordinal, rect) in grid.iter().enumerate() {
            cancelled(cancel)?;
            let planes = &mut samples[..3 * rect.pixels()];
            fill(*rect, planes)?;
            encoded.clear();
            for value in planes.iter() {
                if !value.is_finite() {
                    return Err(Error::validation(format!(
                        "derived source chunk at {},{} holds a non-finite value",
                        rect.x, rect.y
                    )));
                }
                encoded.extend_from_slice(&value.to_le_bytes());
            }
            let sha256 = sha256_hex(&encoded);
            self.publish(
                &sha256,
                &encoded,
                [Step::StageChunk(ordinal), Step::RenameChunk(ordinal)],
            )?;
            entries.push(ChunkEntry {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                bytes: rect.bytes(),
                sha256,
            });
        }
        drop((samples, encoded));
        cancelled(cancel)?;
        let manifest = SourceManifest {
            format: SOURCE_MANIFEST_FORMAT,
            width,
            height,
            colour: DerivedColour::LinearSrgbD65,
            sample: SampleEncoding::F32Le,
            layout: ChunkLayout::PlanarRgb,
            chunk_side: CHUNK_SIDE,
            crop,
            chunks: entries,
        };
        let bytes = serde_json::to_vec(&manifest).map_err(|error| {
            Error::internal(format!("cannot encode a source manifest: {error}"))
        })?;
        if bytes.len() as u64 > MAX_SOURCE_MANIFEST_BYTES {
            return Err(Error::resource_limit(format!(
                "a derived source manifest of {} bytes exceeds {MAX_SOURCE_MANIFEST_BYTES}",
                bytes.len()
            )));
        }
        let sha256 = sha256_hex(&bytes);
        let id = DerivedSourceId::for_hash(&sha256)?;
        self.publish(&sha256, &bytes, [Step::StageManifest, Step::RenameManifest])?;
        inject(Step::SyncDirectory)
            .and_then(|()| atomic_file::sync_dir(&sources))
            .map_err(|error| file_error("cannot publish derived source", error.kind()))?;
        Ok(PublishedSource {
            id,
            manifest_bytes: bytes.len() as u64,
            width,
            height,
            colour: DerivedColour::LinearSrgbD65,
            chunks: manifest
                .chunks
                .into_iter()
                .map(|entry| PublishedChunk {
                    sha256: entry.sha256,
                    bytes: entry.bytes,
                })
                .collect(),
        })
    }

    /// [`Self::write_with`] over whole planes already in memory: `planes` is `width × height` red
    /// values row by row, then green, then blue, as a [`LinearImage`] holds them.
    pub(crate) fn write_planes(
        &self,
        width: u32,
        height: u32,
        planes: &[f32],
        crop: Option<ReferenceCrop>,
        cancel: &AtomicBool,
    ) -> Result<PublishedSource, Error> {
        let (values, plane) = layout(width, height)?;
        if planes.len() != values {
            return Err(Error::validation(format!(
                "a {width} × {height} derived source needs {values} planar values"
            )));
        }
        self.write_with(width, height, crop, cancel, |rect, chunk| {
            copy_rect(planes, plane, width, rect, chunk);
            Ok(())
        })
    }

    /// Make `bytes` the object `sha256`: live first, under the decision lock, so no collection
    /// removes it from then on; then kept when it already holds them, or staged, synced and renamed
    /// over whatever is there. The rename is made durable by the caller's one directory sync.
    fn publish(&self, sha256: &str, bytes: &[u8], steps: [Step; 2]) -> Result<(), Error> {
        {
            let _decision = lock(&SOURCE_DECISIONS);
            self.live.mark(sha256);
        }
        if object_holds(&self.root, sha256, bytes)? {
            return Ok(());
        }
        let staged = self
            .root
            .join(TEMPORARY)
            .join(uuid::Uuid::new_v4().simple().to_string());
        inject(steps[0])
            .and_then(|()| atomic_file::stage(&staged, bytes))
            .map_err(|error| {
                let _ = fs::remove_file(&staged);
                file_error("cannot stage derived source object", error.kind())
            })?;
        inject(steps[1])
            .and_then(|()| fs::rename(&staged, source_object_path(&self.root, sha256)))
            .map_err(|error| {
                let _ = fs::remove_file(&staged);
                file_error("cannot publish derived source object", error.kind())
            })
    }
}

/// Copy the rectangle `rect` of whole planes (`plane` values each, `width` a row) into `chunk`'s
/// three planes.
fn copy_rect(planes: &[f32], plane: usize, width: u32, rect: ChunkRect, chunk: &mut [f32]) {
    let (columns, rows) = (rect.width as usize, rect.height as usize);
    for channel in 0..3 {
        for row in 0..rows {
            let from = channel * plane + (rect.y as usize + row) * width as usize + rect.x as usize;
            let to = (channel * rows + row) * columns;
            chunk[to..to + columns].copy_from_slice(&planes[from..from + columns]);
        }
    }
}

/// One derived source a worker should read and verify: the root and catalog it must belong to and
/// the manifest's recorded length. What the catalog owner hands a worker after statting the
/// objects (`EditorService::derived_source_read`).
#[derive(Clone, Debug)]
pub(crate) struct DerivedSourceRead {
    pub(crate) root: PathBuf,
    pub(crate) catalog_id: String,
    pub(crate) id: DerivedSourceId,
    pub(crate) manifest_bytes: u64,
}

/// A source's manifest whose hash, shape and bounds a worker checked.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedManifest {
    manifest: SourceManifest,
    grid: Vec<ChunkRect>,
}

impl VerifiedManifest {
    pub(crate) fn width(&self) -> u32 {
        self.manifest.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.manifest.height
    }

    pub(crate) fn colour(&self) -> DerivedColour {
        self.manifest.colour
    }

    pub(crate) fn crop(&self) -> Option<ReferenceCrop> {
        self.manifest.crop
    }

    /// Each chunk's hash, in manifest order.
    pub(crate) fn chunk_hashes(&self) -> impl Iterator<Item = &str> {
        self.manifest
            .chunks
            .iter()
            .map(|entry| entry.sha256.as_str())
    }
}

/// The root must be this catalog's: a missing or unmarked root is `source-unavailable`, another
/// catalog's `incompatible`.
fn check_root(read: &DerivedSourceRead) -> Result<(), Error> {
    let root = read.root.display();
    match root_state(&read.root, &read.catalog_id)? {
        RootState::Ready => Ok(()),
        RootState::Absent => Err(Error::source_unavailable(format!(
            "artifact directory {root} is missing; move it with the catalog"
        ))),
        RootState::Unmarked => Err(Error::source_unavailable(format!(
            "artifact directory {root} has no manifest"
        ))),
        RootState::Foreign(detail) => Err(Error::incompatible(detail)),
    }
}

/// Read and verify one source's manifest: the root is this catalog's, the object a file of its own
/// with the recorded length and the hash its identity names, and the document the current format
/// and shape with every bound and chunk place checked. A missing manifest is `source-unavailable:
/// derived source <id> is missing`, another length or hash `... is corrupt`, another format or
/// shape `incompatible`; nothing is ever rewritten. Worker work: at most 64 KiB.
pub(crate) fn read_manifest(read: &DerivedSourceRead) -> Result<VerifiedManifest, Error> {
    let id = &read.id;
    let missing = || Error::source_unavailable(format!("derived source {id} is missing"));
    let corrupt = || Error::source_unavailable(format!("derived source {id} is corrupt"));
    check_root(read)?;
    if sources_dir(&read.root)?.is_none() {
        return Err(missing());
    }
    if read.manifest_bytes > MAX_SOURCE_MANIFEST_BYTES {
        return Err(Error::resource_limit(format!(
            "derived source {id} has a manifest larger than {MAX_SOURCE_MANIFEST_BYTES} bytes"
        )));
    }
    match object_length(&read.root, id.sha256())? {
        None => return Err(missing()),
        Some(length) if length != read.manifest_bytes => return Err(corrupt()),
        Some(_) => {}
    }
    let mut file = match File::open(source_object_path(&read.root, id.sha256())) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(missing()),
        Err(error) => return Err(file_error("cannot read derived source", error.kind())),
        Ok(file) => file,
    };
    let mut bytes = vec![0; read.manifest_bytes as usize];
    let whole = read_exactly(&mut file, &mut bytes)
        .map_err(|error| file_error("cannot read derived source", error.kind()))?;
    if !whole || sha256_hex(&bytes) != id.sha256() {
        return Err(corrupt());
    }
    let manifest = parse_manifest(id, &bytes)?;
    let grid = manifest.check(id)?;
    Ok(VerifiedManifest { manifest, grid })
}

/// The manifest document: its marker first, so a format this build does not know is named as
/// such, then the current shape.
fn parse_manifest(id: &DerivedSourceId, bytes: &[u8]) -> Result<SourceManifest, Error> {
    let incompatible = |reason: String| {
        Error::incompatible(format!(
            "derived source {id} manifest {reason}; the source is kept unchanged"
        ))
    };
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| incompatible(format!("is not valid JSON ({error})")))?;
    match value.get("format").and_then(serde_json::Value::as_u64) {
        Some(format) if format == u64::from(SOURCE_MANIFEST_FORMAT) => {}
        Some(format) => return Err(incompatible(format!("format {format} is not supported"))),
        None => return Err(incompatible("has no format marker".into())),
    }
    serde_json::from_value(value)
        .map_err(|error| incompatible(format!("is not a source manifest ({error})")))
}

/// Read and verify a whole derived source into one [`LinearImage`] allocation: the manifest
/// ([`read_manifest`]), then each chunk in turn through one reusable 12 MiB buffer — a file of its
/// own, of the listed length and hash, every sample finite — copied into its place in the planes.
/// A missing chunk is `source-unavailable: derived source <id> chunk <n> is missing`, another
/// length, hash or a non-finite sample `... is corrupt`; cancellation is checked between chunks.
/// The image is the source's identity view, fingerprinted with the source identity; negative
/// values and values above one are kept exactly. Worker work: one full frame of planes, at most
/// 1.5 GiB, which the caller accounts for, and one chunk of scratch.
pub(crate) fn read_derived_source(
    read: &DerivedSourceRead,
    cancel: &AtomicBool,
) -> Result<LinearImage, Error> {
    cancelled(cancel)?;
    let verified = read_manifest(read)?;
    let (width, height) = (verified.width(), verified.height());
    let (values, plane) = layout(width, height)?;
    let id = &read.id;
    // Every value is written below, chunk by chunk, before the planes are adopted; a zeroed
    // allocation only faults its pages in as they are written.
    let mut planes = vec![0.0_f32; values];
    let mut buffer = Vec::new();
    for (ordinal, (entry, rect)) in verified
        .manifest
        .chunks
        .iter()
        .zip(&verified.grid)
        .enumerate()
    {
        cancelled(cancel)?;
        let missing =
            || Error::source_unavailable(format!("derived source {id} chunk {ordinal} is missing"));
        let corrupt =
            || Error::source_unavailable(format!("derived source {id} chunk {ordinal} is corrupt"));
        match object_length(&read.root, &entry.sha256)? {
            None => return Err(missing()),
            Some(length) if length != entry.bytes => return Err(corrupt()),
            Some(_) => {}
        }
        let mut file = match File::open(source_object_path(&read.root, &entry.sha256)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(missing()),
            Err(error) => return Err(file_error("cannot read derived source", error.kind())),
            Ok(file) => file,
        };
        buffer.resize(entry.bytes as usize, 0);
        let whole = read_exactly(&mut file, &mut buffer)
            .map_err(|error| file_error("cannot read derived source", error.kind()))?;
        if !whole || sha256_hex(&buffer) != entry.sha256 {
            return Err(corrupt());
        }
        let (columns, rows) = (rect.width as usize, rect.height as usize);
        let row_bytes = columns * SAMPLE_BYTES as usize;
        for channel in 0..3 {
            for row in 0..rows {
                let from = (channel * rows + row) * row_bytes;
                let to =
                    channel * plane + (rect.y as usize + row) * width as usize + rect.x as usize;
                let target = &mut planes[to..to + columns];
                for (value, sample) in target
                    .iter_mut()
                    .zip(buffer[from..from + row_bytes].chunks_exact(SAMPLE_BYTES as usize))
                {
                    let decoded =
                        f32::from_le_bytes(sample.try_into().expect("four bytes a sample"));
                    if !decoded.is_finite() {
                        return Err(corrupt());
                    }
                    *value = decoded;
                }
            }
        }
    }
    LinearImage::from_validated_planes(width, height, planes, id.as_str().to_owned())
}

/// Remove every object in `sources/` that `keep` does not name and no writer of this process
/// published (`live`), answering how many went. A name that is not a SHA-256 is never removed.
/// Called by the artifact collection after it checked the root is this catalog's.
pub(crate) fn collect_source_objects(
    root: &Path,
    keep: &HashSet<String>,
    live: &LiveSources,
    cancel: &AtomicBool,
) -> Result<usize, Error> {
    let Some(dir) = sources_dir(root)? else {
        return Ok(0);
    };
    let entries = fs::read_dir(&dir)
        .map_err(|error| file_error("cannot read derived source directory", error.kind()))?;
    let mut removed = 0;
    for entry in entries {
        cancelled(cancel)?;
        let entry = entry
            .map_err(|error| file_error("cannot read derived source directory", error.kind()))?;
        let name = entry.file_name();
        let Some(sha256) = name.to_str().filter(|name| is_sha256(name)) else {
            continue;
        };
        if keep.contains(sha256) {
            continue;
        }
        let _decision = lock(&SOURCE_DECISIONS);
        if live.contains(sha256) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(file_error(
                    "cannot remove derived source object",
                    error.kind(),
                ));
            }
        }
    }
    Ok(removed)
}

/// Whether `root` holds any source object, which keeps an unmarked root from being claimed.
pub(crate) fn holds_objects(root: &Path) -> Result<bool, Error> {
    match fs::read_dir(root.join(SOURCES)) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(file_error(
            "cannot read derived source directory",
            error.kind(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temp(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "luxforge-derived-source-{}-{}-{name}",
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

    /// A deterministic sample with signed values, headroom, a negative zero, the smallest normal,
    /// a subnormal and a large value, so an exact round trip is a statement about every bit.
    pub(crate) fn sample(channel: usize, x: u32, y: u32) -> f32 {
        let mix = (u64::from(x) * 31 + u64::from(y) * 17 + channel as u64 * 7919) % 1000;
        match (x as usize + y as usize + channel) % 13 {
            0 => -0.0,
            1 => f32::MIN_POSITIVE,
            2 => 1.0e-40,
            3 => 262_016.0,
            4 => -3.5,
            _ => (mix as f32 - 300.0) / 97.0,
        }
    }

    /// Whole planes of [`sample`].
    pub(crate) fn planes(width: u32, height: u32) -> Vec<f32> {
        let plane = width as usize * height as usize;
        let mut planes = vec![0.0; 3 * plane];
        for channel in 0..3 {
            for y in 0..height {
                for x in 0..width {
                    planes[channel * plane + (y * width + x) as usize] = sample(channel, x, y);
                }
            }
        }
        planes
    }

    /// Whether two runs of samples are the same bits, negative zeros and subnormals included.
    pub(crate) fn same_bits(read: &[f32], written: &[f32]) -> bool {
        read.len() == written.len()
            && read
                .iter()
                .zip(written)
                .all(|(read, written)| read.to_bits() == written.to_bits())
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

    #[test]
    fn the_chunk_grid_tiles_the_frame_row_by_row_within_the_source_bounds() {
        let grid = chunk_grid(2500, 1100).unwrap();
        let places: Vec<[u32; 4]> = grid
            .iter()
            .map(|rect| [rect.x, rect.y, rect.width, rect.height])
            .collect();
        assert_eq!(
            places,
            [
                [0, 0, 1024, 1024],
                [1024, 0, 1024, 1024],
                [2048, 0, 452, 1024],
                [0, 1024, 1024, 76],
                [1024, 1024, 1024, 76],
                [2048, 1024, 452, 76],
            ]
        );
        assert_eq!(grid[0].bytes(), 12 * 1024 * 1024);
        assert_eq!(grid[5].bytes(), 452 * 76 * 12);
        // The widest frame the 128 MP limit admits at the side limit, every chunk bounded.
        let widest = chunk_grid(16_384, 7_812).unwrap();
        assert_eq!(widest.len(), 16 * 8);
        assert!(widest.iter().all(|rect| rect.bytes() <= MAX_CHUNK_BYTES));
        for (width, height, kind) in [
            (16_385, 1, ErrorKind::ResourceLimit),
            (16_384, 16_384, ErrorKind::ResourceLimit),
            (0, 10, ErrorKind::Validation),
        ] {
            assert_eq!(chunk_grid(width, height).unwrap_err().kind, kind);
        }
    }

    #[test]
    fn a_signed_headroom_source_round_trips_exactly_across_chunks() {
        let directory = temp("round-trip");
        let root = directory.join("catalog.artifacts");
        let live = LiveSources::default();
        let (width, height) = (2100, 1030);
        let values = planes(width, height);
        let crop = ReferenceCrop {
            reference_width: 2200,
            reference_height: 1100,
            x: 40,
            y: 30,
        };
        let published = writer(&root, &live)
            .write_planes(width, height, &values, Some(crop), &never())
            .unwrap();
        published.validate().unwrap();
        assert_eq!(published.chunks.len(), 6);
        assert_eq!(
            published.payload_bytes(),
            u64::from(width) * u64::from(height) * 12
        );
        assert_eq!(
            published.interpretation(),
            DerivedInterpretation {
                source_id: published.id.clone(),
                width,
                height,
                colour: DerivedColour::LinearSrgbD65,
            }
        );
        // Every object is named by its own hash and live, and the staging area is empty.
        let stored = objects(&root);
        assert_eq!(stored.len(), 7);
        for name in &stored {
            let bytes = fs::read(source_object_path(&root, name)).unwrap();
            assert_eq!(&format!("{:x}", Sha256::digest(&bytes)), name);
            assert!(live.contains(name));
        }
        assert_eq!(staged(&root), 0);
        // The manifest states its interpretation explicitly.
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(source_object_path(&root, published.id.sha256())).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["format"], 1);
        assert_eq!(manifest["colour"], "linear-srgb-d65");
        assert_eq!(manifest["sample"], "f32-le");
        assert_eq!(manifest["layout"], "planar-rgb");
        assert_eq!(manifest["chunk_side"], 1024);
        assert_eq!(manifest["crop"]["x"], 40);
        assert_eq!(manifest["chunks"][2]["width"], 52);
        // A chunk's first bytes are its red plane's first samples, little-endian, and its green
        // plane follows the whole red one.
        let first = &published.chunks[0];
        let bytes = fs::read(source_object_path(&root, &first.sha256)).unwrap();
        assert_eq!(bytes[..4], sample(0, 0, 0).to_le_bytes());
        assert_eq!(bytes[4..8], sample(0, 1, 0).to_le_bytes());
        let green = 1024 * 1024 * 4;
        assert_eq!(bytes[green..green + 4], sample(1, 0, 0).to_le_bytes());
        // Read back into one allocation, every bit as written.
        let verified = read_manifest(&reading(&root, &published)).unwrap();
        assert_eq!(verified.crop(), Some(crop));
        assert_eq!(verified.colour(), DerivedColour::LinearSrgbD65);
        assert_eq!((verified.width(), verified.height()), (width, height));
        assert_eq!(
            verified.chunk_hashes().collect::<Vec<_>>(),
            published
                .chunks
                .iter()
                .map(|chunk| chunk.sha256.as_str())
                .collect::<Vec<_>>()
        );
        let image = read_derived_source(&reading(&root, &published), &never()).unwrap();
        assert_eq!((image.width(), image.height()), (width, height));
        assert_eq!(image.fingerprint(), published.id.as_str());
        assert_eq!(image.plane_bytes(), values.len() as u64 * 4);
        assert!(
            same_bits(image.planes(), &values),
            "every sample keeps its bits"
        );
        // The same pixels again are the same source and the same objects.
        let again = writer(&root, &LiveSources::default())
            .write_planes(width, height, &values, Some(crop), &never())
            .unwrap();
        assert_eq!(again, published);
        assert_eq!(objects(&root), stored);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_non_finite_sample_or_a_cancellation_publishes_no_manifest() {
        let directory = temp("refusals");
        let root = directory.join("catalog.artifacts");
        let live = LiveSources::default();
        let (width, height) = (1100, 20);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut values = planes(width, height);
            // In the second chunk's blue plane.
            values[2 * 1100 * 20 + 1050] = bad;
            let error = writer(&root, &live)
                .write_planes(width, height, &values, None, &never())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{error}");
            assert!(error.detail.contains("non-finite"), "{error}");
        }
        // The first chunk was written each time, and nothing names it.
        assert_eq!(objects(&root).len(), 1);
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let error = writer(&root, &live)
            .write_with(width, height, None, &cancel, |_, chunk| {
                calls += 1;
                chunk.fill(0.5);
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(calls, 1, "cancellation is checked between chunks");
        assert_eq!(staged(&root), 0);
        // A crop outside its reference and planes of the wrong length are refused before a write.
        let before = objects(&root);
        let outside = ReferenceCrop {
            reference_width: width,
            reference_height: height,
            x: 1,
            y: 0,
        };
        let values = planes(width, height);
        assert_eq!(
            writer(&root, &live)
                .write_planes(width, height, &values, Some(outside), &never())
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
        assert_eq!(
            writer(&root, &live)
                .write_planes(width, height, &values[1..], None, &never())
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
        assert_eq!(objects(&root), before);
        fs::remove_dir_all(directory).unwrap();
    }

    /// A failure at each write boundary is an error that leaves nothing staged; a retry publishes
    /// the same source, which reads back whole.
    #[test]
    fn a_failure_at_each_write_boundary_fails_the_publication_and_a_retry_completes_it() {
        let (width, height) = (1500, 1100);
        let values = planes(width, height);
        let reference = {
            let directory = temp("boundaries-reference");
            let root = directory.join("catalog.artifacts");
            let published = writer(&root, &LiveSources::default())
                .write_planes(width, height, &values, None, &never())
                .unwrap();
            fs::remove_dir_all(directory).unwrap();
            published
        };
        for step in [
            Step::StageChunk(0),
            Step::RenameChunk(0),
            Step::StageChunk(3),
            Step::RenameChunk(2),
            Step::StageManifest,
            Step::RenameManifest,
            Step::SyncDirectory,
        ] {
            let directory = temp("boundaries");
            let root = directory.join("catalog.artifacts");
            fail::at(step);
            let error = writer(&root, &LiveSources::default())
                .write_planes(width, height, &values, None, &never())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::FileAccess, "{step:?}: {error}");
            assert!(error.detail.contains("derived source"), "{error}");
            assert_eq!(staged(&root), 0, "{step:?} leaves nothing staged");
            let manifest = objects(&root).contains(reference.id.sha256());
            assert_eq!(
                manifest,
                step == Step::SyncDirectory,
                "{step:?}: only an unsynced rename leaves the manifest, which no catalog records"
            );
            let retried = writer(&root, &LiveSources::default())
                .write_planes(width, height, &values, None, &never())
                .unwrap();
            assert_eq!(retried, reference, "{step:?}");
            let image = read_derived_source(&reading(&root, &retried), &never()).unwrap();
            assert!(same_bits(image.planes(), &values));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn missing_corrupt_and_escaping_objects_are_refused_by_name_and_left_as_they_are() {
        let directory = temp("corrupt");
        let root = directory.join("catalog.artifacts");
        let (width, height) = (1030, 1030);
        let values = planes(width, height);
        let published = writer(&root, &LiveSources::default())
            .write_planes(width, height, &values, None, &never())
            .unwrap();
        let read = reading(&root, &published);
        let id = &published.id;
        let chunk = |n: usize| source_object_path(&root, &published.chunks[n].sha256);
        let refused = |expected: ErrorKind, detail: &str| {
            let error = read_derived_source(&read, &never()).unwrap_err();
            assert_eq!(error.kind, expected, "{error}");
            assert!(error.detail.contains(detail), "{detail}: {error}");
        };
        // One flipped bit in the last chunk's last sample.
        let original = fs::read(chunk(3)).unwrap();
        let mut flipped = original.clone();
        *flipped.last_mut().unwrap() ^= 1;
        fs::write(chunk(3), &flipped).unwrap();
        refused(
            ErrorKind::SourceUnavailable,
            &format!("derived source {id} chunk 3 is corrupt"),
        );
        assert_eq!(fs::read(chunk(3)).unwrap(), flipped, "left as it is");
        fs::write(chunk(3), &original[..original.len() - 4]).unwrap();
        refused(ErrorKind::SourceUnavailable, "chunk 3 is corrupt");
        fs::remove_file(chunk(3)).unwrap();
        refused(ErrorKind::SourceUnavailable, "chunk 3 is missing");
        fs::write(chunk(3), &original).unwrap();
        #[cfg(unix)]
        {
            // A chunk that is a link, even to the same bytes, is not followed.
            let elsewhere = directory.join("elsewhere");
            fs::write(&elsewhere, &original).unwrap();
            fs::remove_file(chunk(3)).unwrap();
            std::os::unix::fs::symlink(&elsewhere, chunk(3)).unwrap();
            refused(
                ErrorKind::Incompatible,
                "is not a file of the artifact root",
            );
            fs::remove_file(chunk(3)).unwrap();
            fs::write(chunk(3), &original).unwrap();
            // Nor is a `sources/` that links out of the root.
            let moved = directory.join("moved-sources");
            fs::rename(root.join(SOURCES), &moved).unwrap();
            std::os::unix::fs::symlink(&moved, root.join(SOURCES)).unwrap();
            refused(
                ErrorKind::Incompatible,
                "is not a directory of the artifact root",
            );
            fs::remove_file(root.join(SOURCES)).unwrap();
            fs::rename(&moved, root.join(SOURCES)).unwrap();
        }
        // The manifest: other bytes of its length, another length, gone.
        let manifest = source_object_path(&root, id.sha256());
        let text = fs::read(&manifest).unwrap();
        let mut other = text.clone();
        other[0] = b' ';
        fs::write(&manifest, &other).unwrap();
        refused(
            ErrorKind::SourceUnavailable,
            &format!("derived source {id} is corrupt"),
        );
        fs::write(&manifest, [text.as_slice(), b" "].concat()).unwrap();
        refused(ErrorKind::SourceUnavailable, "is corrupt");
        fs::remove_file(&manifest).unwrap();
        refused(
            ErrorKind::SourceUnavailable,
            &format!("derived source {id} is missing"),
        );
        fs::write(&manifest, &text).unwrap();
        let image = read_derived_source(&read, &never()).unwrap();
        assert!(same_bits(image.planes(), &values));
        // A root that is another catalog's is neither read nor written.
        let foreign = DerivedSourceRead {
            catalog_id: "catalog-b".into(),
            ..read.clone()
        };
        let error = read_derived_source(&foreign, &never()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert!(
            error.detail.contains("belongs to catalog catalog-a"),
            "{error}"
        );
        let error =
            DerivedSourceWriter::new(root.clone(), "catalog-b".into(), LiveSources::default())
                .write_planes(width, height, &values, None, &never())
                .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        let cancel = AtomicBool::new(true);
        assert_eq!(
            read_derived_source(&read, &cancel).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        fs::remove_dir_all(directory).unwrap();
    }

    /// A manifest of another format, shape or grid is `incompatible` by name and kept: each is
    /// stored under its own hash, as a writer would store it, so only its contents are wrong.
    #[test]
    fn a_manifest_of_another_format_shape_or_grid_is_refused_and_kept() {
        let directory = temp("manifests");
        let root = directory.join("catalog.artifacts");
        let published = writer(&root, &LiveSources::default())
            .write_planes(1100, 4, &planes(1100, 4), None, &never())
            .unwrap();
        let good: serde_json::Value = serde_json::from_slice(
            &fs::read(source_object_path(&root, published.id.sha256())).unwrap(),
        )
        .unwrap();
        let changed = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut value = good.clone();
            change(&mut value);
            value
        };
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (
                changed(&|m| m["format"] = 2.into()),
                "format 2 is not supported",
            ),
            (
                changed(&|m| {
                    m.as_object_mut().unwrap().remove("format");
                }),
                "has no format marker",
            ),
            (
                changed(&|m| m["sample"] = "f32-be".into()),
                "is not a source manifest",
            ),
            (
                changed(&|m| m["colour"] = "display-p3".into()),
                "is not a source manifest",
            ),
            (
                changed(&|m| m["layout"] = "interleaved-rgb".into()),
                "is not a source manifest",
            ),
            (
                changed(&|m| m["extra"] = true.into()),
                "is not a source manifest",
            ),
            (
                changed(&|m| m["chunk_side"] = 512.into()),
                "uses 512 pixel chunks",
            ),
            (
                changed(&|m| m["width"] = 16_385.into()),
                "dimensions are out of bounds",
            ),
            (
                changed(&|m| {
                    m["chunks"].as_array_mut().unwrap().pop();
                }),
                "lists 1 chunks where its dimensions need 2",
            ),
            (
                changed(&|m| m["chunks"][1]["x"] = 1000.into()),
                "chunk 1 does not match its place",
            ),
            (
                changed(&|m| m["chunks"][0]["bytes"] = 4.into()),
                "chunk 0 does not match its place",
            ),
            (
                changed(&|m| m["chunks"][1]["sha256"] = "../../catalog.sqlite".into()),
                "chunk 1 does not name a SHA-256",
            ),
            (
                changed(&|m| {
                    m["crop"] = serde_json::json!({
                        "reference_width": 1100, "reference_height": 4, "x": 1, "y": 0
                    })
                }),
                "crop must lie within its reference frame",
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
            let error = read_derived_source(&read, &never()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible, "{reason}: {error}");
            assert!(error.detail.contains(reason), "{reason}: {error}");
            assert!(error.detail.contains("kept unchanged"), "{error}");
            assert_eq!(fs::read(source_object_path(&root, &sha256)).unwrap(), bytes);
        }
        // A manifest larger than the bound is refused before it is read.
        let oversized = DerivedSourceRead {
            manifest_bytes: MAX_SOURCE_MANIFEST_BYTES + 1,
            ..reading(&root, &published)
        };
        assert_eq!(
            read_manifest(&oversized).unwrap_err().kind,
            ErrorKind::ResourceLimit
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn collection_removes_only_objects_neither_retained_nor_live_and_never_other_names() {
        let directory = temp("collect");
        let root = directory.join("catalog.artifacts");
        let live = LiveSources::default();
        let kept = writer(&root, &LiveSources::default())
            .write_planes(1030, 2, &planes(1030, 2), None, &never())
            .unwrap();
        let orphan = writer(&root, &LiveSources::default())
            .write_planes(3, 3, &[7.0; 27], None, &never())
            .unwrap();
        let fresh = writer(&root, &live)
            .write_planes(2, 2, &[-1.0; 12], None, &never())
            .unwrap();
        let stranger = root.join(SOURCES).join("not-a-hash");
        fs::write(&stranger, b"left alone").unwrap();
        let keep: HashSet<String> = std::iter::once(kept.id.sha256().to_owned())
            .chain(kept.chunks.iter().map(|chunk| chunk.sha256.clone()))
            .collect();
        let removed = collect_source_objects(&root, &keep, &live, &never()).unwrap();
        assert_eq!(removed, 2, "the orphan's manifest and its one chunk");
        let left = objects(&root);
        assert!(keep.iter().all(|name| left.contains(name)));
        assert!(left.contains(fresh.id.sha256()), "live");
        assert!(!left.contains(orphan.id.sha256()));
        assert!(stranger.exists());
        assert!(read_derived_source(&reading(&root, &kept), &never()).is_ok());
        fs::remove_dir_all(directory).unwrap();
    }

    /// A photo-sized source past the 256 MiB one artifact may hold is written through bounded chunk
    /// objects and read back bit for bit: 6144 × 4096 is 288 MiB of samples in 24 chunks of at
    /// most 12 MiB. Generated chunk by chunk, so only the frame read back is ever whole. Not a
    /// performance measurement.
    #[test]
    fn a_source_larger_than_one_artifact_round_trips_through_bounded_chunks() {
        let directory = temp("large");
        let root = directory.join("catalog.artifacts");
        let (width, height) = (6144_u32, 4096_u32);
        let published = writer(&root, &LiveSources::default())
            .write_with(width, height, None, &never(), |rect, chunk| {
                let pixels = rect.pixels();
                for channel in 0..3 {
                    for row in 0..rect.height {
                        for column in 0..rect.width {
                            chunk[channel * pixels + (row * rect.width + column) as usize] =
                                sample(channel, rect.x + column, rect.y + row);
                        }
                    }
                }
                Ok(())
            })
            .unwrap();
        assert!(published.payload_bytes() > super::super::MAX_ARTIFACT_BYTES);
        assert_eq!(published.chunks.len(), 24);
        for chunk in &published.chunks {
            assert!(chunk.bytes <= MAX_CHUNK_BYTES);
            let stored = fs::metadata(source_object_path(&root, &chunk.sha256)).unwrap();
            assert_eq!(stored.len(), chunk.bytes);
        }
        let image = read_derived_source(&reading(&root, &published), &never()).unwrap();
        let plane = (width * height) as usize;
        let planes = image.planes();
        let exact = (0..3).all(|channel| {
            (0..height).all(|y| {
                (0..width).all(|x| {
                    planes[channel * plane + (y * width + x) as usize].to_bits()
                        == sample(channel, x, y).to_bits()
                })
            })
        });
        assert!(exact, "every sample keeps its bits");
        drop(image);
        fs::remove_dir_all(directory).unwrap();
    }
}
