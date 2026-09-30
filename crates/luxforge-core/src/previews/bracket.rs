//! The preview check for brackets the metadata cannot show (P5; `docs/design/catalog.md`,
//! "Grouping inside a view"): a small fingerprint of each file's grid tier, kept beside the tier's
//! row, and a measure over a run's fingerprints that organizing asks through [`BracketProbe`].
//!
//! - **The fingerprint** ([`Fingerprint`], [`FINGERPRINT_BYTES`] = 581 bytes, fixed). A complete
//!   grid tier (embedded or developed, never the thumbnail stage) decoded at the DCT scale that
//!   leaves at least two samples a cell on its short side (a quarter of a 512 × 341 tier) and
//!   reduced to a [`GRID`] × [`GRID`] grid over the upright frame: each cell's mean log2 Rec. 709
//!   luminance of its usable pixels in 1/20 EV codes, or unusable when more than an eighth of its
//!   pixels are clipped (a channel at [`CLIP_CODE`] or more) or dark (every channel below
//!   [`DARK_CODE`]), where the log no longer follows the exposure. Clipped and dark pixels are
//!   excluded, not weighted, and two frames are compared only on the cells both keep, so a sky
//!   blown in the brighter frame drops out of their comparison rather than pulling its step.
//!   Before the cells, the layout and the tier's size. [`Store::write`] makes it from the tier's
//!   own JPEG as it writes a complete grid tier, so every path that makes one gives it one, and
//!   keeps it in `grid_fingerprints` naming the tier's file: valid while the file's grid row names
//!   that file and is valid for the file's signature, as the tier is.
//! - **The measure** ([`measure`]). The frames are put in order of their centre-weighted mean log
//!   luminance ([`centre_weights`]), and each is compared with the next brighter one over the
//!   cells usable in both. A comparison answers `None`, and so the run, unless at least
//!   [`MIN_COVERAGE`] of the weight is usable in both; the framing is unchanged, the cosine of the
//!   two frames' log-luminance gradient fields at least [`FRAMING_MIN`], each with at least
//!   [`MIN_STRUCTURE_EV`] of structure to compare; and the step is consistent, all but
//!   [`MAX_DISAGREEING`] of the weight moving within [`AGREEMENT`] of it, since an exposure step
//!   moves every cell the same way and light that changes over part of the frame does not. The
//!   step is the weighted mean difference of the cells that agree with the weighted median one.
//!   A run is `O(frames × cells)`: for 9 frames, 8 comparisons of 576 cells, with every working
//!   array on the stack (under 10 KB) and the answer its one allocation.
//! - **The probe** ([`PreviewProbe`], [`bracket_probe`]). Organizing asks it only about runs the
//!   metadata cannot classify, and it reads just that run's fingerprints, in one indexed query
//!   through a cached statement, so a view whose runs the metadata classifies reads none; it never
//!   reads a photograph's file or decodes.
//!
//! **Calibration** (the tests' `bracket_preview_calibration`: six synthetic scenes through their
//! grid tiers' JPEGs, brackets within ±2 EV). Without a tone curve, as the generated folders
//! render, every link measures 0.97 to 1.01 times its step. Through three camera-like curves
//! (Narkowicz's ACES fit, Hable's and Reinhard's) links measure 0.56 to 1.50 times their step, a
//! shoulder compressing the brighter links and a contrasty toe expanding the darker ones, with
//! framing cosines of 0.90 or more and at most 0.01 of the weight disagreeing; every run is
//! measured. At one exposure, a pan of 2% of the frame keeps a cosine of 0.79 to 0.83, 3% 0.55 to
//! 0.78 and 5% 0.70 or less; a zoom of 5% 0.80 to 0.95 and of 20% 0.68 or less. A cloud's shadow
//! over 30 to 70% of the frame, or the sun going in, leaves 0.23 to 0.51 of the weight disagreeing,
//! or measures under 0.42 EV. Light that changes alike over four fifths of the frame or more moves
//! nearly every cell alike and is measured as a step: the limit of what brightness alone can tell.
//!
//! [`Store::write`]: super::cache::Store::write
use super::{FILE_GRID_SIDE, cache};
use crate::{
    Error,
    catalog_types::{BracketProbe, FileId, PreviewOrigin, ViewItem},
    colour::{luma::rec709, srgb},
};
use luxforge_jpeg::{Decoder, Limits, Scale};
use rusqlite::Connection;
use std::cell::Cell;
#[cfg(test)]
use std::collections::HashMap;

/// Cells on each side of a fingerprint's grid: a cell is about 21 px of a 512 px grid tier, and a
/// pan of 2% of the frame half a cell.
const GRID: usize = 24;
const CELLS: usize = GRID * GRID;
/// The fingerprint's layout, its first byte; a blob of another layout is no fingerprint.
const LAYOUT: u8 = 1;
/// The layout byte, then the tier's width and height, each a little-endian `u16`.
const HEADER: usize = 5;
/// A fingerprint's size in bytes.
const FINGERPRINT_BYTES: usize = HEADER + CELLS;

/// A cell whose pixels are mostly dark.
const DARK: u8 = 0;
/// A cell whose pixels are mostly clipped.
const CLIPPED: u8 = 255;
/// Codes a cell's log2 luminance is quantized at: 1/20 EV.
const CODES_PER_EV: f32 = 20.0;
/// The log2 luminance of code 0; codes 1 to 254 span −11.95 to 0.7 EV, which holds every
/// luminance an 8-bit pixel with a channel at [`DARK_CODE`] or more has (−11.4 EV at least).
const LOG_FLOOR: f32 = -12.0;
/// A pixel with a channel at or above this code is clipped: its luminance no longer follows the
/// exposure.
const CLIP_CODE: u8 = 250;
/// A pixel whose every channel is below this code is dark: one code there is 0.12 EV or more of
/// luminance, and a JPEG's noise several codes.
const DARK_CODE: u8 = 16;
/// A cell is unusable when more than one pixel in this many is clipped or dark: the mean of the
/// rest would then leave out the tail an exposure step moves most.
const EXCLUDED_SHARE: u32 = 8;
/// The grid tier's long edge at most: a JPEG larger than a grid tier is not fingerprinted.
const MAX_SIDE: u32 = FILE_GRID_SIDE;

/// A run of at most this many frames is measured: a bracket's most
/// ([`Thresholds::bracket_max_frames`](crate::catalog_types::Thresholds) is at most 9).
const MAX_FRAMES: usize = 9;
/// A comparison needs at least this share of the centre-weighted grid usable in both frames. The
/// calibration's brackets within ±2 EV kept 0.29 or more, the brighter frame's sky blown.
const MIN_COVERAGE: f32 = 0.2;
/// Two frames' framing is the same when the cosine of their log-luminance gradient fields, over the
/// cells usable in both, is at least this: every calibration bracket kept 0.90 or more, and a pan
/// of 3% of the frame falls to 0.55 to 0.78.
const FRAMING_MIN: f32 = 0.8;
/// …and when each field has at least this root-mean-square gradient between neighbouring cells, in
/// EV: a frame with less (fog, a plain wall) has too little structure to confirm its framing.
const MIN_STRUCTURE_EV: f32 = 0.05;
/// A cell agrees with a step when its difference lies between these shares of the step, widened
/// by the last figure in EV either way: a camera's tone curve moves each cell by its own share of
/// the step, less in its shoulder and more where it adds contrast, but always the same way, while
/// light that changes over part of the frame leaves the rest where it was.
const AGREEMENT: (f32, f32, f32) = (0.35, 2.5, 0.15);
/// A step is consistent when at most this share of the usable weight disagrees with it: the
/// calibration's brackets reached 0.01, changing light over 30 to 70% of the frame 0.23 or more.
const MAX_DISAGREEING: f32 = 0.2;

/// A grid tier's brightness and structure, [`FINGERPRINT_BYTES`] bytes (see the module
/// documentation).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Fingerprint([u8; FINGERPRINT_BYTES]);

impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (width, height) = self.size();
        write!(f, "Fingerprint({width}×{height})")
    }
}

impl Fingerprint {
    /// The fingerprint of a grid tier's JPEG: decoded at the most reducing DCT scale that keeps
    /// two samples a cell on its short side. `None` for bytes that do not decode, a JPEG past a
    /// grid tier's size, or one smaller than the grid.
    pub(crate) fn of_jpeg(jpeg: &[u8]) -> Option<Self> {
        let limits = Limits {
            max_side: MAX_SIDE,
            max_pixels: u64::from(MAX_SIDE) * u64::from(MAX_SIDE),
        };
        let mut decoder = Decoder::new(jpeg, limits).ok()?;
        let frame = (decoder.width(), decoder.height());
        let samples = 2 * GRID as u32;
        decoder
            .set_scale(Scale::covering(frame, (samples, samples)))
            .ok()?;
        let (width, height) = (decoder.width(), decoder.height());
        // At most the grid tier's size at full scale: a tier whose short side is under twice the
        // grid is decoded whole, one under 512 × 95 px.
        let mut rgba = vec![0; width as usize * height as usize * 4];
        decoder.read_rows(&mut rgba).ok()?;
        decoder.finish().ok()?;
        Self::of_rgba(frame, width, height, &rgba)
    }

    /// The fingerprint of an upright frame of size `frame`, from `width` × `height` RGBA8 pixels of
    /// it (the frame itself or a reduced decode). `None` when the pixels are fewer than the grid on
    /// either side.
    pub(crate) fn of_rgba(frame: (u32, u32), width: u32, height: u32, rgba: &[u8]) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        if w < GRID || h < GRID || rgba.len() != w * h * 4 {
            return None;
        }
        let to_linear = srgb::decode_table();
        // Per cell: the sum of usable pixels' log2 luminance, how many were usable, and how many
        // were clipped and dark.
        let mut sums = [0.0f32; CELLS];
        let mut usable = [0u32; CELLS];
        let mut clipped = [0u32; CELLS];
        let mut dark = [0u32; CELLS];
        for y in 0..h {
            let row = y * GRID / h * GRID;
            for (x, pixel) in rgba[y * w * 4..(y + 1) * w * 4].chunks_exact(4).enumerate() {
                let cell = row + x * GRID / w;
                let rgb = [pixel[0], pixel[1], pixel[2]];
                let brightest = rgb[0].max(rgb[1]).max(rgb[2]);
                if brightest >= CLIP_CODE {
                    clipped[cell] += 1;
                } else if brightest < DARK_CODE {
                    dark[cell] += 1;
                } else {
                    sums[cell] += rec709(srgb::decode_pixel_in(to_linear, rgb)).log2();
                    usable[cell] += 1;
                }
            }
        }
        let mut bytes = [0u8; FINGERPRINT_BYTES];
        bytes[0] = LAYOUT;
        bytes[1..3].copy_from_slice(&(frame.0.min(u32::from(u16::MAX)) as u16).to_le_bytes());
        bytes[3..5].copy_from_slice(&(frame.1.min(u32::from(u16::MAX)) as u16).to_le_bytes());
        for cell in 0..CELLS {
            let total = usable[cell] + clipped[cell] + dark[cell];
            let excluded = clipped[cell] + dark[cell];
            bytes[HEADER + cell] = if excluded * EXCLUDED_SHARE > total {
                if clipped[cell] >= dark[cell] {
                    CLIPPED
                } else {
                    DARK
                }
            } else {
                let log = sums[cell] / usable[cell] as f32;
                ((log - LOG_FLOOR) * CODES_PER_EV).round().clamp(1.0, 254.0) as u8
            };
        }
        Some(Self(bytes))
    }

    /// A stored fingerprint; `None` for a blob of another size or layout.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let bytes: [u8; FINGERPRINT_BYTES] = bytes.try_into().ok()?;
        (bytes[0] == LAYOUT).then_some(Self(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The tier's width and height.
    fn size(&self) -> (u16, u16) {
        (
            u16::from_le_bytes([self.0[1], self.0[2]]),
            u16::from_le_bytes([self.0[3], self.0[4]]),
        )
    }

    fn code(&self, cell: usize) -> u8 {
        self.0[HEADER + cell]
    }

    /// A cell's log2 luminance, when it is usable.
    fn log(&self, cell: usize) -> Option<f32> {
        match self.code(cell) {
            DARK | CLIPPED => None,
            code => Some(f32::from(code) / CODES_PER_EV + LOG_FLOOR),
        }
    }

    /// Whether `other` is of a frame with the same shape, within 1%: a frame of another shape (a
    /// turned camera, another crop) is another framing.
    fn same_shape(&self, other: &Self) -> bool {
        let (a, b) = (self.size(), other.size());
        let across = f64::from(a.0) * f64::from(b.1);
        let down = f64::from(b.0) * f64::from(a.1);
        (across - down).abs() <= 0.01 * across.max(down)
    }

    /// The centre-weighted mean log2 luminance of the usable cells; `None` when none is.
    fn mean_log(&self, weights: &[f32; CELLS]) -> Option<f32> {
        let (mut sum, mut weight) = (0.0, 0.0);
        for (cell, w) in weights.iter().enumerate() {
            if let Some(log) = self.log(cell) {
                sum += w * log;
                weight += w;
            }
        }
        (weight > 0.0).then(|| sum / weight)
    }
}

/// Each cell's weight: 1 at the centre, falling to ½ at the middle of an edge and ⅓ in a corner
/// (`1 / (1 + u² + v²)` with `u` and `v` from −1 to 1 across the frame), since the subject is
/// usually central and a slightly moved frame changes its edges first.
fn centre_weights() -> [f32; CELLS] {
    let mut weights = [0.0; CELLS];
    let at = |index: usize| (index as f32 + 0.5) / GRID as f32 * 2.0 - 1.0;
    for (cell, weight) in weights.iter_mut().enumerate() {
        let (u, v) = (at(cell % GRID), at(cell / GRID));
        *weight = 1.0 / (1.0 + u * u + v * v);
    }
    weights
}

/// Each frame's exposure in stops relative to the first (brighter positive, the first 0), when
/// every frame has the same framing and each step is consistent across the frame; `None` otherwise,
/// and for fewer than 2 or more than [`MAX_FRAMES`] frames. See the module documentation. The work
/// is `O(frames × cells)` on the stack; the answer is the one allocation.
pub(crate) fn measure(frames: &[&Fingerprint]) -> Option<Vec<f32>> {
    let count = frames.len();
    if !(2..=MAX_FRAMES).contains(&count) || frames.iter().any(|frame| !frame.same_shape(frames[0]))
    {
        return None;
    }
    let weights = centre_weights();
    let mut brightness = [0.0f32; MAX_FRAMES];
    for (at, frame) in frames.iter().enumerate() {
        brightness[at] = frame.mean_log(&weights)?;
    }
    let mut order: [usize; MAX_FRAMES] = std::array::from_fn(|at| at);
    order[..count].sort_by(|a, b| brightness[*a].total_cmp(&brightness[*b]).then(a.cmp(b)));
    let mut exposure = [0.0f32; MAX_FRAMES];
    for pair in order[..count].windows(2) {
        let (darker, brighter) = (pair[0], pair[1]);
        exposure[brighter] = exposure[darker] + step(frames[darker], frames[brighter], &weights)?;
    }
    Some(
        exposure[..count]
            .iter()
            .map(|exposure_at| exposure_at - exposure[0])
            .collect(),
    )
}

/// Whether a cell whose log luminance changed by `difference` agrees with `step` under
/// `agreement` ([`AGREEMENT`]).
fn agrees(difference: f32, step: f32, agreement: (f32, f32, f32)) -> bool {
    let (low, high, floor) = agreement;
    let (a, b) = (low * step, high * step);
    (a.min(b) - floor..=a.max(b) + floor).contains(&difference)
}

/// `b`'s exposure less `a`'s in stops, over the cells usable in both, when their framing is the
/// same and the step is consistent across the frame.
fn step(a: &Fingerprint, b: &Fingerprint, weights: &[f32; CELLS]) -> Option<f32> {
    let comparison = compare(a, b, weights)?;
    (comparison.coverage >= MIN_COVERAGE
        && comparison
            .framing
            .is_some_and(|cosine| cosine >= FRAMING_MIN)
        && comparison.disagreeing <= MAX_DISAGREEING)
        .then_some(comparison.step)
}

/// What comparing two frames found.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Comparison {
    /// The share of the centre-weighted grid usable in both.
    coverage: f32,
    /// The cosine of their gradient fields, or `None` when either has too little structure.
    framing: Option<f32>,
    /// The step: the weighted mean difference over the cells that agree with the weighted median.
    step: f32,
    /// The share of the usable weight that disagrees with the step.
    disagreeing: f32,
}

/// Compare `a` with `b` over the cells usable in both; `None` when no cell is.
fn compare(a: &Fingerprint, b: &Fingerprint, weights: &[f32; CELLS]) -> Option<Comparison> {
    compare_with(a, b, weights, AGREEMENT)
}

/// [`compare`] with another agreement band, for calibration.
fn compare_with(
    a: &Fingerprint,
    b: &Fingerprint,
    weights: &[f32; CELLS],
    agreement: (f32, f32, f32),
) -> Option<Comparison> {
    let mut differences = [0.0f32; CELLS];
    let mut usable = [false; CELLS];
    // The usable weight by difference in codes, −253 to 253: codes are whole, so their weighted
    // median needs no sort.
    let mut by_difference = [0.0f32; 2 * 254 + 1];
    let (mut total, mut covered) = (0.0, 0.0);
    for cell in 0..CELLS {
        total += weights[cell];
        if a.log(cell).is_some() && b.log(cell).is_some() {
            let codes = i32::from(b.code(cell)) - i32::from(a.code(cell));
            differences[cell] = codes as f32 / CODES_PER_EV;
            usable[cell] = true;
            covered += weights[cell];
            by_difference[(codes + 254) as usize] += weights[cell];
        }
    }
    if covered <= 0.0 {
        return None;
    }
    // The difference at half the usable weight (the cells' weighted median, an estimator of the
    // step, not a timing statistic), then the mean over the cells that agree with it: cells that
    // changed on their own (a moving subject, part of the frame in a cloud's shadow) neither pull
    // the step nor, when they are nearly half, drag it between the two.
    let mut below = 0.0;
    let middle = by_difference
        .iter()
        .position(|weight| {
            below += weight;
            below >= covered / 2.0
        })
        .map_or(0.0, |at| (at as f32 - 254.0) / CODES_PER_EV);
    let (mut sum, mut weight) = (0.0, 0.0);
    for cell in (0..CELLS).filter(|cell| usable[*cell]) {
        if agrees(differences[cell], middle, agreement) {
            sum += weights[cell] * differences[cell];
            weight += weights[cell];
        }
    }
    let step = if weight > 0.0 { sum / weight } else { middle };
    let disagreeing: f32 = (0..CELLS)
        .filter(|cell| usable[*cell] && !agrees(differences[*cell], step, agreement))
        .map(|cell| weights[cell])
        .sum();
    Some(Comparison {
        coverage: covered / total,
        framing: framing(a, b, &usable),
        step,
        disagreeing: disagreeing / covered,
    })
}

/// The cosine of `a`'s and `b`'s log-luminance gradients between neighbouring cells usable in
/// both, across and down: 1 for the same framing, whatever the exposure, since an exposure step
/// alone changes no log-luminance gradient. `None` when either has less than
/// [`MIN_STRUCTURE_EV`] of structure to compare.
fn framing(a: &Fingerprint, b: &Fingerprint, usable: &[bool; CELLS]) -> Option<f32> {
    let (mut dot, mut aa, mut bb, mut pairs) = (0.0f32, 0.0f32, 0.0f32, 0u32);
    let mut add = |from: usize, to: usize| {
        if usable[from] && usable[to] {
            let (Some(a0), Some(a1), Some(b0), Some(b1)) =
                (a.log(from), a.log(to), b.log(from), b.log(to))
            else {
                return;
            };
            let (ga, gb) = (a1 - a0, b1 - b0);
            dot += ga * gb;
            aa += ga * ga;
            bb += gb * gb;
            pairs += 1;
        }
    };
    for y in 0..GRID {
        for x in 0..GRID {
            let cell = y * GRID + x;
            if x + 1 < GRID {
                add(cell, cell + 1);
            }
            if y + 1 < GRID {
                add(cell, cell + GRID);
            }
        }
    }
    let structure = MIN_STRUCTURE_EV * MIN_STRUCTURE_EV * pairs as f32;
    (pairs > 0 && aa >= structure && bb >= structure).then(|| dot / (aa * bb).sqrt())
}

/// What organizing asks about a run the metadata cannot classify ([`BracketProbe`]), answered
/// from the run's grid fingerprints. It reads them lazily, one run at a time: organizing asks only
/// about runs of 2 to [`MAX_FRAMES`] frames whose metadata does not vary, so a view whose runs the
/// metadata classifies reads none, and a burst's run reads its own frames' fingerprints and no
/// others, in one indexed query ([`bracket_probe`]).
pub(crate) struct PreviewProbe<'c> {
    fingerprints: Fingerprints<'c>,
    /// Frames looked up so far, whatever was found.
    asked: Cell<usize>,
}

/// Where a probe reads fingerprints.
enum Fingerprints<'c> {
    /// The index, a run at a time, through one cached statement.
    Index(&'c Connection),
    /// A map, for tests that measure without an index.
    #[cfg(test)]
    Memory(HashMap<FileId, (PreviewOrigin, Fingerprint)>),
}

impl PreviewProbe<'_> {
    /// How many frames it has looked up so far: none for a view whose runs the metadata
    /// classifies.
    #[allow(dead_code, reason = "lane D's browse.view reports it as it lands")]
    pub(crate) fn frames_asked(&self) -> usize {
        self.asked.get()
    }

    /// A probe over fingerprints held in memory.
    #[cfg(test)]
    pub(crate) fn memory(
        fingerprints: HashMap<FileId, (PreviewOrigin, Fingerprint)>,
    ) -> PreviewProbe<'static> {
        PreviewProbe {
            fingerprints: Fingerprints::Memory(fingerprints),
            asked: Cell::new(0),
        }
    }

    /// The fingerprints of `files`' grid tiers, in order, `None` for a file without one: a file
    /// with no grid tier yet, only its thumbnail stage, a stale row, or a row written before the
    /// fingerprint existed.
    fn load(&self, files: &[FileId]) -> Result<Vec<Option<(PreviewOrigin, Fingerprint)>>, Error> {
        self.asked.set(self.asked.get() + files.len());
        let connection = match &self.fingerprints {
            Fingerprints::Index(connection) => *connection,
            #[cfg(test)]
            Fingerprints::Memory(map) => {
                return Ok(files.iter().map(|file| map.get(file).cloned()).collect());
            }
        };
        let mut found = vec![None; files.len()];
        if files.is_empty() {
            return Ok(found);
        }
        let ids = serde_json::to_string(&files.iter().map(|file| file.0).collect::<Vec<_>>())
            .map_err(|error| Error::internal(format!("file ids: {error}")))?;
        let mut statement = connection.prepare_cached(
            "SELECT f.id, f.byte_len, f.modified_ns, f.device, f.inode,
                    p.byte_len, p.modified_ns, p.device, p.inode, p.origin, g.fingerprint
             FROM json_each(?1) j
             JOIN files f ON f.id = j.value
             JOIN previews p ON p.file_id = f.id AND p.tier = 'grid'
             JOIN grid_fingerprints g ON g.file_id = f.id AND g.path = p.path",
        )?;
        let mut rows = statement.query([ids])?;
        while let Some(row) = rows.next()? {
            let current = cache::signature(row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?);
            let recorded = cache::signature(row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?);
            if current != recorded {
                continue;
            }
            let file = FileId(row.get(0)?);
            let origin: String = row.get(9)?;
            let bytes: Vec<u8> = row.get(10)?;
            if let (Some(at), Some(origin), Some(fingerprint)) = (
                files.iter().position(|asked| *asked == file),
                PreviewOrigin::parse(&origin),
                Fingerprint::from_bytes(&bytes),
            ) {
                found[at] = Some((origin, fingerprint));
            }
        }
        Ok(found)
    }

    /// The fingerprints of `files` as [`Self::load`] finds them, keyed by file, for tests.
    #[cfg(test)]
    pub(crate) fn loaded(&self, files: &[FileId]) -> HashMap<FileId, (PreviewOrigin, Fingerprint)> {
        let found = self.load(files).unwrap();
        files
            .iter()
            .zip(found)
            .filter_map(|(file, found)| found.map(|found| (*file, found)))
            .collect()
    }
}

impl BracketProbe for PreviewProbe<'_> {
    /// [`measure`] over the frames' fingerprints, read for this run alone, when every frame is a
    /// file with a fingerprint and all of one origin: a camera's embedded preview and a neutral
    /// development differ in brightness by more than a bracket's step. A photograph, or an index
    /// that cannot be read, answers `None`: the run stays a burst.
    fn measure(&self, frames: &[ViewItem]) -> Option<Vec<f32>> {
        if !(2..=MAX_FRAMES).contains(&frames.len()) {
            return None;
        }
        let mut files = [FileId(0); MAX_FRAMES];
        for (slot, item) in files.iter_mut().zip(frames) {
            let ViewItem::File(file) = item else {
                return None;
            };
            *slot = *file;
        }
        let found = self.load(&files[..frames.len()]).ok()?;
        let origin = found.first()?.as_ref()?.0;
        let fingerprints = found
            .iter()
            .map(|found| {
                found
                    .as_ref()
                    .filter(|(frame_origin, _)| *frame_origin == origin)
                    .map(|(_, fingerprint)| fingerprint)
            })
            .collect::<Option<Vec<&Fingerprint>>>()?;
        measure(&fingerprints)
    }
}

/// The preview bracket probe over the index behind `connection`, for lane D's `browse.view` to
/// hand to `organize::group`: it reads nothing until organizing asks about a run, and then only
/// that run's fingerprints (the fingerprints of files' grid tiers whose row is valid for the
/// file's current signature).
pub(crate) fn bracket_probe(connection: &Connection) -> PreviewProbe<'_> {
    PreviewProbe {
        fingerprints: Fingerprints::Index(connection),
        asked: Cell::new(0),
    }
}

#[cfg(test)]
#[path = "bracket/tests.rs"]
mod bracket_preview;
