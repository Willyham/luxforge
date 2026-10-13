//! Paged derived sources (`docs/design/panorama-merge.md`, "Large derived sources"): a derived
//! source past the camera limits, up to 65,000 pixels a side and 512 megapixels, stored as a
//! pyramid of chunk grids and never allocated whole.
//!
//! - **Levels.** Level 0 is the full-resolution grid of [`CHUNK_SIDE`] chunks, as a resident
//!   source's. Each further level halves the one before, rounding up, down to the first whose long
//!   side is at most [`OVERVIEW_LONG_SIDE`] ([`level_shapes`]): each of its pixels is the 2 × 2
//!   area average of the pixels of the level before that it covers ([`area_average`]), one, two or
//!   four of them at a right or bottom edge. Chunk origins are multiples of the even chunk side, so
//!   every 2 × 2 block lies in one chunk of the level before.
//! - **Manifest format 2** ([`PAGED_MANIFEST_FORMAT`]) lists the levels, level 0 first, each with
//!   its dimensions and its chunks exactly as a resident manifest lists its one grid. At most
//!   [`MAX_LEVEL0_CHUNKS`] chunks at level 0 and [`MAX_PAGED_CHUNKS`] over every level; the
//!   manifest is at most 1 MiB. The catalog records every level's chunks, level 0 first, as the
//!   source's chunks, so retention, collection and Empty Removed cover all of them.
//! - **Writing** ([`PagedSourceWrite`]) takes level-0 chunks as a producer emits them, in any
//!   order, each encoded, hashed and published at once; finishing builds each further level in a
//!   streaming pass over the stored chunks of the level before, read back and verified, and writes
//!   the manifest last. Scratch: at most [`WRITER_SCRATCH_BYTES`], never a level whole.
//! - **Reading** ([`PagedSource`]) verifies the manifest and reads the smallest level whole, the
//!   overview, on preparation. Windows of any level, with halos, are assembled from the chunks
//!   they touch through a bounded, verified chunk cache of at most [`CHUNK_CACHE_BYTES`], charged
//!   to the source worker's plane gate. The largest level within the camera limits is the
//!   resident level, read whole on request and held like an ordinary source. A damaged chunk
//!   fails only what reads it, naming its level and ordinal.
use super::{
    CHUNK_SIDE, ChunkEntry, ChunkLayout, ChunkRect, DerivedColour, DerivedSourceId,
    DerivedSourceRead, DerivedSourceWriter, ObjectRead, PublishedChunk, PublishedSource,
    ReferenceCrop, SAMPLE_BYTES, SampleEncoding, SourceClass, VerifiedLevel, VerifiedManifest,
    cancelled, check_crop, check_entries, corrupt_chunk, decode_samples, grid_of,
    incompatible_manifest, read_chunk, read_object,
};
use crate::{
    Error,
    source::{CAMERA_SOURCE, LinearImage, PlaneGate, PlaneLease, PlaneLimits},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError, atomic::AtomicBool},
};

/// A paged source's manifest shape. A resident source's is
/// [`super::RESIDENT_MANIFEST_FORMAT`].
pub(crate) const PAGED_MANIFEST_FORMAT: u32 = 2;
/// The long side at or below which a level is the last: the overview a preparation reads whole.
pub(crate) const OVERVIEW_LONG_SIDE: u32 = 4096;
/// The most level-0 chunks a paged source holds: a 65,000-pixel side is 64 chunks.
pub(crate) const MAX_LEVEL0_CHUNKS: usize = 4096;
/// The most chunks a paged source holds over every level: level 0's 4,096 and a third more.
pub(crate) const MAX_PAGED_CHUNKS: usize = 5462;
/// The most bytes one paged source's chunk cache keeps: the most one resident source may hold.
pub(crate) const CHUNK_CACHE_BYTES: u64 = luxforge_raw::MAX_RGB_BYTES as u64;
/// The most scratch a paged writer holds: a chunk of samples, its encoding and one chunk of the
/// level before, read back, below this.
pub(crate) const WRITER_SCRATCH_BYTES: u64 = 48 * 1024 * 1024;
/// The bound of one window's planes: any width a level has, and at most the planes of a camera
/// development, 128 megapixels and 1.5 GiB, which its caller accounts for.
pub(crate) const WINDOW: PlaneLimits = PlaneLimits {
    name: "paged source window",
    max_side: crate::source::PAGED_DERIVED_SOURCE.max_side,
    ..CAMERA_SOURCE
};

/// The refusal of a path that needs a derived source's whole frame for a paged source, which has
/// none: what that path reads is not available until it reads the source's levels and windows.
/// Typed (`resource-limit`), never a fallback to allocating the frame.
pub(crate) fn no_whole_frame(what: &str, id: &DerivedSourceId) -> Error {
    Error::resource_limit(format!(
        "{what} is not available for paged derived source {id}, which has no whole frame; it is \
         read through its levels and windows"
    ))
}

/// The dimensions of a paged `width` × `height` source's levels, level 0 first: each the one
/// before halved, rounding up, down to the first whose long side is at most
/// [`OVERVIEW_LONG_SIDE`].
pub(crate) fn level_shapes(width: u32, height: u32) -> Vec<(u32, u32)> {
    let mut shapes = vec![(width, height)];
    let (mut w, mut h) = (width, height);
    while w.max(h) > OVERVIEW_LONG_SIDE {
        (w, h) = (w.div_ceil(2), h.div_ceil(2));
        shapes.push((w, h));
    }
    shapes
}

/// The value of one pixel of a further level from the `count` (one to four) pixels of the level
/// before that it covers, `samples` in the order top-left, top-right, bottom-left, bottom-right
/// of those present: their sum in binary64, in that order from the first, divided by `count` and
/// rounded to the nearest binary32, so one pixel alone keeps its exact value, a negative zero
/// included. The mean of finite binary32 values is finite and within their range.
#[inline]
pub(crate) fn area_average(samples: &[f32]) -> f32 {
    let (first, rest) = samples
        .split_first()
        .expect("a block covers one pixel at least");
    let sum = rest
        .iter()
        .fold(f64::from(*first), |sum, &value| sum + f64::from(value));
    (sum / samples.len() as f64) as f32
}

/// How a further level is made from the level before. The one reduction so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Reduction {
    #[serde(rename = "area-average-2x2")]
    AreaAverage2x2,
}

/// A paged source's manifest as stored, its marker first. `deny_unknown_fields` and the closed
/// enumerations make every other shape `incompatible`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PagedManifest {
    format: u32,
    width: u32,
    height: u32,
    colour: DerivedColour,
    sample: SampleEncoding,
    layout: ChunkLayout,
    chunk_side: u32,
    reduction: Reduction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crop: Option<ReferenceCrop>,
    levels: Vec<LevelManifest>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LevelManifest {
    width: u32,
    height: u32,
    chunks: Vec<ChunkEntry>,
}

impl PagedManifest {
    /// Everything the shape cannot say: dimensions past the resident bound and within the paged
    /// one, the chunk side, the crop within its reference, the levels exactly [`level_shapes`],
    /// and each level's chunks exactly its grid, each with its length and a well-formed hash.
    pub(super) fn check(self, id: &DerivedSourceId) -> Result<VerifiedManifest, Error> {
        let refuse = |reason: String| incompatible_manifest(id, reason);
        if self.chunk_side != CHUNK_SIDE {
            return Err(refuse(format!(
                "uses {} pixel chunks, not {CHUNK_SIDE}",
                self.chunk_side
            )));
        }
        match SourceClass::of(self.width, self.height) {
            Ok(SourceClass::Paged) => {}
            Ok(SourceClass::Resident) => {
                return Err(refuse(format!(
                    "is format {PAGED_MANIFEST_FORMAT} for {} × {}, within the resident limits",
                    self.width, self.height
                )));
            }
            Err(error) => {
                return Err(refuse(format!(
                    "dimensions are out of bounds ({})",
                    error.detail
                )));
            }
        }
        if let Some(crop) = self.crop {
            check_crop(crop, self.width, self.height).map_err(|error| refuse(error.detail))?;
        }
        let shapes = level_shapes(self.width, self.height);
        if self.levels.len() != shapes.len() {
            return Err(refuse(format!(
                "lists {} levels where its dimensions need {}",
                self.levels.len(),
                shapes.len()
            )));
        }
        let mut total = 0;
        for (index, (level, &(width, height))) in self.levels.iter().zip(&shapes).enumerate() {
            if (level.width, level.height) != (width, height) {
                return Err(refuse(format!(
                    "level {index} is {} × {} where its dimensions need {width} × {height}",
                    level.width, level.height
                )));
            }
            check_entries(&level.chunks, &grid_of(width, height), |ordinal| {
                format!("chunk {ordinal}")
            })
            .map_err(|reason| refuse(format!("level {index} {reason}")))?;
            total += level.chunks.len();
        }
        if self.levels[0].chunks.len() > MAX_LEVEL0_CHUNKS || total > MAX_PAGED_CHUNKS {
            return Err(refuse(format!("lists {total} chunks, past the bound")));
        }
        Ok(VerifiedManifest {
            class: SourceClass::Paged,
            width: self.width,
            height: self.height,
            colour: self.colour,
            crop: self.crop,
            levels: self
                .levels
                .into_iter()
                .map(|level| VerifiedLevel {
                    width: level.width,
                    height: level.height,
                    chunks: level.chunks,
                })
                .collect(),
        })
    }
}

impl DerivedSourceWriter {
    /// Begin writing a paged `width` × `height` source, whose dimensions must be past the
    /// resident bound and within the paged one (a resident source is written by
    /// [`Self::write_with`]), and whose crop must lie within its reference. Claims the root as
    /// [`Self::write_with`] does; nothing is published until the first chunk is put.
    pub(crate) fn begin_paged(
        &self,
        width: u32,
        height: u32,
        crop: Option<ReferenceCrop>,
        cancel: &AtomicBool,
    ) -> Result<PagedSourceWrite, Error> {
        if SourceClass::of(width, height)? != SourceClass::Paged {
            return Err(Error::validation(format!(
                "a {width} × {height} derived source is within the resident limits and is \
                 written whole"
            )));
        }
        if let Some(crop) = crop {
            check_crop(crop, width, height)?;
        }
        cancelled(cancel)?;
        let sources = self.claim_sources()?;
        let grid = grid_of(width, height);
        Ok(PagedSourceWrite {
            writer: self.clone(),
            sources,
            width,
            height,
            crop,
            level0: vec![None; grid.len()],
            grid,
            encoded: Vec::new(),
        })
    }
}

/// One paged source being written: level-0 chunks put as a producer emits them, then
/// [`Self::finish`]. Dropping it unfinished publishes no manifest, so nothing can name the chunks
/// it wrote, which a collection removes once they are no longer live.
pub(crate) struct PagedSourceWrite {
    writer: DerivedSourceWriter,
    sources: PathBuf,
    width: u32,
    height: u32,
    crop: Option<ReferenceCrop>,
    grid: Vec<ChunkRect>,
    level0: Vec<Option<ChunkEntry>>,
    /// The reusable encoding of one chunk.
    pub(super) encoded: Vec<u8>,
}

impl PagedSourceWrite {
    /// The level-0 grid, row by row and left to right: what each ordinal of [`Self::put`] names.
    pub(crate) fn grid(&self) -> &[ChunkRect] {
        &self.grid
    }

    /// Publish level-0 chunk `ordinal` from its three `planes` — its red values row by row, then
    /// green, then blue — encoded, hashed, staged, synced and renamed at once, in whatever order a
    /// producer emits them. A chunk outside the grid, one already put, planes of another length or
    /// a non-finite sample are refused (`validation`) and publish nothing.
    pub(crate) fn put(
        &mut self,
        ordinal: usize,
        planes: &[f32],
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        cancelled(cancel)?;
        let Some(&rect) = self.grid.get(ordinal) else {
            return Err(Error::validation(format!(
                "a paged derived source has {} level-0 chunks, not {}",
                self.grid.len(),
                ordinal + 1
            )));
        };
        if self.level0[ordinal].is_some() {
            return Err(Error::validation(format!(
                "level-0 chunk {ordinal} of a paged derived source is already written"
            )));
        }
        if planes.len() != 3 * rect.pixels() {
            return Err(Error::validation(format!(
                "level-0 chunk {ordinal} needs {} planar values",
                3 * rect.pixels()
            )));
        }
        let entry = self
            .writer
            .put_chunk(rect, planes, ordinal, &mut self.encoded)?;
        self.level0[ordinal] = Some(entry);
        Ok(())
    }

    /// Build every further level from the stored chunks of the level before — each level-k
    /// chunk from the at most four chunks of level k − 1 it covers, read back one at a time and
    /// verified — publish their chunks, then the manifest, and sync `sources/` once. Every level-0
    /// chunk must have been put (`validation`). Cancellation is checked between chunks, and a
    /// failure leaves no manifest. Scratch: at most [`WRITER_SCRATCH_BYTES`].
    pub(crate) fn finish(mut self, cancel: &AtomicBool) -> Result<PublishedSource, Error> {
        let missing = self.level0.iter().filter(|entry| entry.is_none()).count();
        if missing > 0 {
            return Err(Error::validation(format!(
                "a paged derived source is missing {missing} of its level-0 chunks"
            )));
        }
        let shapes = level_shapes(self.width, self.height);
        let mut levels = vec![LevelManifest {
            width: self.width,
            height: self.height,
            chunks: self.level0.drain(..).flatten().collect(),
        }];
        let mut ordinal = levels[0].chunks.len();
        let mut planes = Vec::new();
        let mut read_back = Vec::new();
        for (index, &(width, height)) in shapes.iter().enumerate().skip(1) {
            let previous = &levels[index - 1];
            let columns = previous.width.div_ceil(CHUNK_SIDE) as usize;
            let rows = previous.height.div_ceil(CHUNK_SIDE) as usize;
            let mut chunks = Vec::new();
            for rect in grid_of(width, height) {
                cancelled(cancel)?;
                planes.clear();
                planes.resize(3 * rect.pixels(), 0.0_f32);
                let (column, row) = (
                    (rect.x / CHUNK_SIDE) as usize * 2,
                    (rect.y / CHUNK_SIDE) as usize * 2,
                );
                for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    if column + dx >= columns || row + dy >= rows {
                        continue;
                    }
                    let source = &previous.chunks[(row + dy) * columns + column + dx];
                    self.read_back(index - 1, source, &mut read_back)?;
                    reduce_into(source.rect(), &read_back, rect, &mut planes);
                }
                chunks.push(
                    self.writer
                        .put_chunk(rect, &planes, ordinal, &mut self.encoded)?,
                );
                #[cfg(test)]
                scratch::record(
                    chunk_bytes(&planes)
                        + read_back.capacity() as u64
                        + self.encoded.capacity() as u64,
                );
                ordinal += 1;
            }
            levels.push(LevelManifest {
                width,
                height,
                chunks,
            });
        }
        drop((planes, read_back, std::mem::take(&mut self.encoded)));
        cancelled(cancel)?;
        let manifest = PagedManifest {
            format: PAGED_MANIFEST_FORMAT,
            width: self.width,
            height: self.height,
            colour: DerivedColour::LinearSrgbD65,
            sample: SampleEncoding::F32Le,
            layout: ChunkLayout::PlanarRgb,
            chunk_side: CHUNK_SIDE,
            reduction: Reduction::AreaAverage2x2,
            crop: self.crop,
            levels,
        };
        let bytes = serde_json::to_vec(&manifest).map_err(|error| {
            Error::internal(format!("cannot encode a source manifest: {error}"))
        })?;
        let (id, manifest_bytes) = self.writer.publish_manifest(&self.sources, &bytes)?;
        Ok(PublishedSource {
            id,
            manifest_bytes,
            width: self.width,
            height: self.height,
            colour: DerivedColour::LinearSrgbD65,
            chunks: manifest
                .levels
                .into_iter()
                .flat_map(|level| level.chunks)
                .map(|entry| PublishedChunk {
                    sha256: entry.sha256,
                    bytes: entry.bytes,
                })
                .collect(),
        })
    }

    /// Read back a chunk this writer published at `level`, verified, into `buffer`: a chunk that
    /// is gone or changed since is a write failure naming its level.
    fn read_back(
        &self,
        level: usize,
        entry: &ChunkEntry,
        buffer: &mut Vec<u8>,
    ) -> Result<(), Error> {
        match read_object(&self.writer.root, entry, buffer)? {
            ObjectRead::Verified => Ok(()),
            ObjectRead::Missing | ObjectRead::Corrupt => Err(Error::file_access(format!(
                "a level-{level} chunk of a paged derived source changed while its next level \
                 was built"
            ))),
        }
    }
}

/// The most scratch a paged writer held on this thread, for the tests that hold it to
/// [`WRITER_SCRATCH_BYTES`].
#[cfg(test)]
pub(crate) mod scratch {
    use std::cell::Cell;

    thread_local! {
        static PEAK: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn record(bytes: u64) {
        PEAK.set(PEAK.get().max(bytes));
    }

    /// The most recorded since this thread last asked.
    pub(crate) fn take_peak() -> u64 {
        PEAK.replace(0)
    }
}

/// Reduce one stored chunk of a level, `from` and its verified bytes, into the planes of the chunk
/// `to` of the next level, which it covers a quadrant of: each pixel of the quadrant the
/// [`area_average`] of the 2 × 2 block (fewer at an edge) of `from` it covers.
fn reduce_into(from: ChunkRect, bytes: &[u8], to: ChunkRect, planes: &mut [f32]) {
    let (columns, rows) = (from.width as usize, from.height as usize);
    let (left, top) = ((from.x / 2 - to.x) as usize, (from.y / 2 - to.y) as usize);
    let (out_columns, out_rows) = (to.width as usize, to.height as usize);
    let (pairs, odd) = (columns / 2, columns % 2 == 1);
    // Two decoded rows of the source at a time: verified against its hash, which named finite
    // samples when they were written.
    let mut upper = vec![0.0_f32; columns];
    let mut lower = vec![0.0_f32; columns];
    let decode = |row: &mut [f32], channel: usize, y: usize| {
        let at = (channel * rows + y) * columns * SAMPLE_BYTES as usize;
        let source = &bytes[at..at + columns * SAMPLE_BYTES as usize];
        for (value, sample) in row
            .iter_mut()
            .zip(source.chunks_exact(SAMPLE_BYTES as usize))
        {
            *value = f32::from_le_bytes(sample.try_into().expect("four bytes a sample"));
        }
    };
    for channel in 0..3 {
        for y in 0..rows.div_ceil(2) {
            decode(&mut upper, channel, 2 * y);
            let two_rows = 2 * y + 1 < rows;
            if two_rows {
                decode(&mut lower, channel, 2 * y + 1);
            }
            let start = (channel * out_rows + top + y) * out_columns + left;
            let out = &mut planes[start..start + columns.div_ceil(2)];
            // The [`area_average`] of each block, written out for its shapes: top-left, top-right,
            // bottom-left, bottom-right summed in that order in binary64 and rounded once.
            if two_rows {
                for (x, value) in out[..pairs].iter_mut().enumerate() {
                    let sum = f64::from(upper[2 * x])
                        + f64::from(upper[2 * x + 1])
                        + f64::from(lower[2 * x])
                        + f64::from(lower[2 * x + 1]);
                    *value = (sum / 4.0) as f32;
                }
                if odd {
                    let sum = f64::from(upper[columns - 1]) + f64::from(lower[columns - 1]);
                    out[pairs] = (sum / 2.0) as f32;
                }
            } else {
                for (x, value) in out[..pairs].iter_mut().enumerate() {
                    let sum = f64::from(upper[2 * x]) + f64::from(upper[2 * x + 1]);
                    *value = (sum / 2.0) as f32;
                }
                if odd {
                    out[pairs] = upper[columns - 1];
                }
            }
        }
    }
}

/// A window to read from a paged source: the rectangle `[x, y, width, height]` of level `level`,
/// grown by `halo` pixels on every side as far as the level reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowRequest {
    pub(crate) level: usize,
    pub(crate) rect: [u32; 4],
    pub(crate) halo: u32,
}

/// A window a paged source assembled: the pixels of `rect` of level `level` — the requested
/// rectangle `core` grown by its halo and clamped to the level — as three planes, red values row
/// by row, then green, then blue, `rect`'s width a row. Exactly the values the level holds there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PagedWindow {
    pub(crate) level: usize,
    /// What was read, in the level's pixels.
    pub(crate) rect: [u32; 4],
    /// What was asked for, in the level's pixels: within `rect`.
    pub(crate) core: [u32; 4],
    pub(crate) planes: Vec<f32>,
}

impl PagedWindow {
    /// The window as an identity-view [`LinearImage`] fingerprinted `fingerprint`, when it is
    /// within the camera limits a whole allocation keeps to. Its values are verified finite.
    pub(crate) fn into_image(self, fingerprint: String) -> Result<LinearImage, Error> {
        let [_, _, width, height] = self.rect;
        LinearImage::from_validated_planes(width, height, self.planes, fingerprint)
    }
}

/// A paged source as it is prepared: its verified manifest and its overview, the smallest level,
/// read whole; every other read goes through its chunk cache or reads a level whole on request.
/// Shared between threads by its holder; every method takes `&self`.
pub(crate) struct PagedSource {
    read: DerivedSourceRead,
    manifest: VerifiedManifest,
    overview: LinearImage,
    cache: Mutex<ChunkCache>,
    resident: Mutex<Option<LinearImage>>,
    gate: Option<Arc<PlaneGate>>,
}

impl std::fmt::Debug for PagedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PagedSource")
            .field("id", &self.read.id)
            .field("width", &self.manifest.width)
            .field("height", &self.manifest.height)
            .field("levels", &self.manifest.levels.len())
            .finish_non_exhaustive()
    }
}

/// The chunks a paged source keeps, most recently used kept longest, within [`Self::budget`] and
/// what the plane gate admits.
struct ChunkCache {
    chunks: HashMap<(usize, usize), CachedChunk>,
    bytes: u64,
    peak: u64,
    clock: u64,
    budget: u64,
    /// The gate's hold on the chunks kept, charged and discharged as they come and go.
    lease: Option<PlaneLease>,
}

struct CachedChunk {
    planes: Arc<Vec<f32>>,
    used: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ChunkCache {
    fn new(budget: u64) -> Self {
        Self {
            chunks: HashMap::new(),
            bytes: 0,
            peak: 0,
            clock: 0,
            budget,
            lease: None,
        }
    }

    fn get(&mut self, key: (usize, usize)) -> Option<Arc<Vec<f32>>> {
        self.clock += 1;
        let clock = self.clock;
        self.chunks.get_mut(&key).map(|chunk| {
            chunk.used = clock;
            Arc::clone(&chunk.planes)
        })
    }

    /// Drop the least recently used chunk, answering whether there was one.
    fn evict_one(&mut self) -> bool {
        let Some(key) = self
            .chunks
            .iter()
            .min_by_key(|(_, chunk)| chunk.used)
            .map(|(key, _)| *key)
        else {
            return false;
        };
        let chunk = self.chunks.remove(&key).expect("a listed chunk");
        let bytes = chunk_bytes(&chunk.planes);
        self.bytes -= bytes;
        if let Some(lease) = &self.lease {
            lease.discharge(bytes);
        }
        true
    }

    fn clear(&mut self) {
        while self.evict_one() {}
    }

    /// Keep `planes` as `key`, evicting the least recently used until it fits the budget and the
    /// gate admits it; a chunk that cannot fit is read through and not kept.
    fn insert(&mut self, key: (usize, usize), planes: &Arc<Vec<f32>>) {
        let bytes = chunk_bytes(planes);
        if bytes > self.budget || self.chunks.contains_key(&key) {
            return;
        }
        while self.bytes + bytes > self.budget {
            self.evict_one();
        }
        while self
            .lease
            .as_ref()
            .is_some_and(|lease| !lease.try_charge(bytes))
        {
            if !self.evict_one() {
                return;
            }
        }
        self.clock += 1;
        self.chunks.insert(
            key,
            CachedChunk {
                planes: Arc::clone(planes),
                used: self.clock,
            },
        );
        self.bytes += bytes;
        self.peak = self.peak.max(self.bytes);
    }
}

fn chunk_bytes(planes: &[f32]) -> u64 {
    std::mem::size_of_val(planes) as u64
}

impl PagedSource {
    /// Open a paged source whose manifest `manifest` was read and verified from `read`: read its
    /// smallest level whole, verified, as the overview. Nothing else is read, and level 0 is never
    /// allocated. Worker work: the overview, at most 4,096 pixels on its long side (192 MiB), and
    /// one chunk of scratch.
    pub(crate) fn open(
        read: DerivedSourceRead,
        manifest: VerifiedManifest,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        if manifest.class != SourceClass::Paged {
            return Err(Error::internal("a resident source opened as a paged one"));
        }
        let overview = read_level(&read, &manifest, manifest.levels.len() - 1, cancel)?;
        Ok(Self {
            read,
            manifest,
            overview,
            cache: Mutex::new(ChunkCache::new(CHUNK_CACHE_BYTES)),
            resident: Mutex::new(None),
            gate: None,
        })
    }

    /// Make the source worker's plane gate count this source: its overview's planes, every chunk
    /// its cache keeps and its resident level once read. Called once, on the worker, before the
    /// source is shared.
    pub(crate) fn hold(&mut self, gate: &Arc<PlaneGate>) {
        self.overview.hold(gate.lease());
        lock(&self.cache).lease = Some(gate.lease());
        self.gate = Some(Arc::clone(gate));
    }

    pub(crate) fn id(&self) -> &DerivedSourceId {
        &self.read.id
    }

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

    /// Each level's dimensions, level 0 first.
    pub(crate) fn levels(&self) -> Vec<(u32, u32)> {
        self.manifest
            .levels
            .iter()
            .map(|level| (level.width, level.height))
            .collect()
    }

    /// The smallest level, read whole on preparation: at most 4,096 pixels on its long side.
    pub(crate) fn overview(&self) -> &LinearImage {
        &self.overview
    }

    /// Which level the overview is: the last.
    pub(crate) fn overview_level(&self) -> usize {
        self.manifest.levels.len() - 1
    }

    /// Which level is the resident one: the largest within the camera limits. Never level 0,
    /// which is past them.
    pub(crate) fn resident_level_index(&self) -> usize {
        self.manifest
            .levels
            .iter()
            .position(|level| CAMERA_SOURCE.check(level.width, level.height).is_ok())
            .unwrap_or(self.overview_level())
    }

    /// The resident level ([`Self::resident_level_index`]), read whole and verified on first
    /// request and held from then on like an ordinary source's planes, counted by the plane gate
    /// when the source is held. When the gate would not admit its planes beside what it counts,
    /// the chunk cache is emptied first; when it still would not, `resource-limit`. Worker work:
    /// at most 1.5 GiB of planes and one chunk of scratch.
    pub(crate) fn resident_level(&self, cancel: &AtomicBool) -> Result<LinearImage, Error> {
        let mut resident = lock(&self.resident);
        if let Some(image) = resident.as_ref() {
            return Ok(image.clone());
        }
        let level = self.resident_level_index();
        let shape = &self.manifest.levels[level];
        let bytes = u64::from(shape.width) * u64::from(shape.height) * 3 * SAMPLE_BYTES;
        if let Some(gate) = &self.gate
            && !gate.admits(bytes)
        {
            lock(&self.cache).clear();
            if !gate.admits(bytes) {
                return Err(Error::resource_limit(format!(
                    "the resident level of paged derived source {} needs {bytes} bytes the \
                     prepared-source gate does not admit",
                    self.read.id
                )));
            }
        }
        let mut image = read_level(&self.read, &self.manifest, level, cancel)?;
        if let Some(gate) = &self.gate {
            image.hold(gate.lease());
        }
        *resident = Some(image.clone());
        Ok(image)
    }

    /// The resident level when it has been read ([`Self::resident_level`]), without reading it:
    /// what a path on the owner asks, which never reads a level.
    pub(crate) fn resident_now(&self) -> Option<LinearImage> {
        lock(&self.resident).clone()
    }

    /// Read a window of any level: `request`'s rectangle, which must lie within its level, grown
    /// by its halo as far as the level reaches, assembled from the chunks it touches through the
    /// chunk cache. Each chunk is verified when it is read; a damaged one fails this read, naming
    /// it (`source-unavailable: derived source <id> level <l> chunk <n> is corrupt`), and is not
    /// kept. The window's planes are bounded by [`WINDOW`]; cancellation is checked between
    /// chunks.
    pub(crate) fn window(
        &self,
        request: WindowRequest,
        cancel: &AtomicBool,
    ) -> Result<PagedWindow, Error> {
        let WindowRequest { level, rect, halo } = request;
        let Some(shape) = self.manifest.levels.get(level) else {
            return Err(Error::validation(format!(
                "paged derived source {} has {} levels, not {}",
                self.read.id,
                self.manifest.levels.len(),
                level + 1
            )));
        };
        let [x, y, width, height] = rect;
        let inside = width > 0
            && height > 0
            && x.checked_add(width)
                .is_some_and(|right| right <= shape.width)
            && y.checked_add(height)
                .is_some_and(|bottom| bottom <= shape.height);
        if !inside {
            return Err(Error::validation(format!(
                "window {rect:?} does not lie within level {level} ({} × {})",
                shape.width, shape.height
            )));
        }
        let (left, top) = (x.saturating_sub(halo), y.saturating_sub(halo));
        let right = x
            .saturating_add(width)
            .saturating_add(halo)
            .min(shape.width);
        let bottom = y
            .saturating_add(height)
            .saturating_add(halo)
            .min(shape.height);
        let outer = [left, top, right - left, bottom - top];
        let (values, plane) = WINDOW.layout(outer[2], outer[3])?;
        let mut planes = vec![0.0_f32; values];
        let columns = shape.width.div_ceil(CHUNK_SIDE) as usize;
        for row in (top / CHUNK_SIDE)..bottom.div_ceil(CHUNK_SIDE) {
            for column in (left / CHUNK_SIDE)..right.div_ceil(CHUNK_SIDE) {
                cancelled(cancel)?;
                let ordinal = row as usize * columns + column as usize;
                let chunk = self.chunk(level, ordinal)?;
                let from = shape.chunks[ordinal].rect();
                copy_overlap(&chunk, from, &mut planes, plane, outer);
            }
        }
        Ok(PagedWindow {
            level,
            rect: outer,
            core: rect,
            planes,
        })
    }

    /// Chunk `ordinal` of `level`, decoded: from the cache, or read, verified and kept.
    fn chunk(&self, level: usize, ordinal: usize) -> Result<Arc<Vec<f32>>, Error> {
        if let Some(planes) = lock(&self.cache).get((level, ordinal)) {
            return Ok(planes);
        }
        let entry = &self.manifest.levels[level].chunks[ordinal];
        let name = self.manifest.chunk_name(level, ordinal);
        let mut buffer = Vec::new();
        read_chunk(&self.read, &name, entry, &mut buffer)?;
        let mut planes = Vec::new();
        decode_samples(&buffer, &mut planes)
            .then_some(())
            .ok_or_else(|| corrupt_chunk(&self.read.id, &name))?;
        let planes = Arc::new(planes);
        lock(&self.cache).insert((level, ordinal), &planes);
        Ok(planes)
    }

    /// The bytes the chunk cache keeps now, and the most it has kept.
    pub(crate) fn cached_bytes(&self) -> (u64, u64) {
        let cache = lock(&self.cache);
        (cache.bytes, cache.peak)
    }

    /// Bound the chunk cache to `bytes` from now on, below [`CHUNK_CACHE_BYTES`], evicting what
    /// no longer fits: for a caller with a tighter budget, and the tests.
    pub(crate) fn limit_cache(&self, bytes: u64) {
        let mut cache = lock(&self.cache);
        cache.budget = bytes.min(CHUNK_CACHE_BYTES);
        while cache.bytes > cache.budget {
            cache.evict_one();
        }
    }
}

/// Copy the part of the chunk `from` (its decoded planes `chunk`) that overlaps the window `outer`
/// into the window's planes, `plane` values each.
fn copy_overlap(chunk: &[f32], from: ChunkRect, planes: &mut [f32], plane: usize, outer: [u32; 4]) {
    let [left, top, width, height] = outer;
    let x0 = from.x.max(left);
    let x1 = (from.x + from.width).min(left + width);
    let y0 = from.y.max(top);
    let y1 = (from.y + from.height).min(top + height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (columns, rows) = (from.width as usize, from.height as usize);
    let run = (x1 - x0) as usize;
    for channel in 0..3 {
        for y in y0..y1 {
            let source =
                (channel * rows + (y - from.y) as usize) * columns + (x0 - from.x) as usize;
            let target =
                channel * plane + (y - top) as usize * width as usize + (x0 - left) as usize;
            planes[target..target + run].copy_from_slice(&chunk[source..source + run]);
        }
    }
}

/// Read level `level` of a paged source whole, verified chunk by chunk, into one [`LinearImage`]
/// fingerprinted `<id>/level-<n>`: a level within the camera limits only (`resource-limit`
/// otherwise), never level 0.
fn read_level(
    read: &DerivedSourceRead,
    manifest: &VerifiedManifest,
    level: usize,
    cancel: &AtomicBool,
) -> Result<LinearImage, Error> {
    let shape = &manifest.levels[level];
    let (values, plane) = CAMERA_SOURCE.layout(shape.width, shape.height)?;
    let mut planes = vec![0.0_f32; values];
    let mut buffer = Vec::new();
    let outer = [0, 0, shape.width, shape.height];
    let mut decoded = Vec::new();
    for (ordinal, entry) in shape.chunks.iter().enumerate() {
        cancelled(cancel)?;
        let name = manifest.chunk_name(level, ordinal);
        read_chunk(read, &name, entry, &mut buffer)?;
        if !decode_samples(&buffer, &mut decoded) {
            return Err(corrupt_chunk(&read.id, &name));
        }
        copy_overlap(&decoded, entry.rect(), &mut planes, plane, outer);
    }
    LinearImage::from_validated_planes(
        shape.width,
        shape.height,
        planes,
        format!("{}/level-{level}", read.id),
    )
}

#[cfg(test)]
#[path = "paged_source_tests.rs"]
mod tests;
