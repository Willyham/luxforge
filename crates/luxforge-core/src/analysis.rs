//! The standalone exact RGB histogram reducer and output-clipping predicates.
//!
//! This is a pure function over an already-rendered byte raster: it needs no host, job or module
//! change and creates neither a recipe effect nor a history entry (see the [histogram and clipping
//! contract](../../../docs/design/basic-and-histogram.md#histogram-and-clipping-contract) and the
//! integration contract's "Analysis jobs and identity"). It reduces the **rendered SDR sRGB
//! output** of a composition, not scene-linear or RAW/sensor data.

mod jobs;
mod mask_overlay;
mod overlay;

pub use jobs::{AnalysisDomain, AnalysisIdentity};
pub(crate) use jobs::{AnalysisJob, AnalysisQueue};
pub(crate) use mask_overlay::coverage_grid_region;
pub use mask_overlay::{
    MASK_COVERAGE_FULL, MASK_COVERAGE_NONE, MaskOverlay, MaskPixels, coverage_grid,
    quantize_coverage,
};
pub use mask_overlay::{MaskInputGrid, MaskInputPixel};
pub(crate) use overlay::cell_pixel;
pub use overlay::{
    MAX_OVERLAY_CELLS, OVERLAY_BOTH, OVERLAY_HIGHLIGHT, OVERLAY_NONE, OVERLAY_SHADOW, overlay,
};

#[cfg(test)]
use crate::ErrorKind;
use crate::{Cancel, Error};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// The histogram and clipping contract's declared output domain: the rendered SDR sRGB output of
/// the full current composition, after crop and edits, before UI overlays or display scaling. Not
/// the camera/RAW histogram.
pub(crate) const DOMAIN: &str = "srgb-8bit-output";

/// A report is bounded to 16 KiB of counters/metadata before protocol encoding (the histogram and
/// clipping contract, and "Resource and responsiveness constraints").
#[cfg(test)]
pub(crate) const REPORT_BOUND_BYTES: usize = 16 * 1024;

pub(crate) fn deserialize_domain<'de, D>(deserializer: D) -> Result<&'static str, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value == DOMAIN {
        Ok(DOMAIN)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported analysis domain {value:?}, expected {DOMAIN:?}"
        )))
    }
}

/// Serde only implements `Serialize`/`Deserialize` for arrays generically up to length 32; a
/// 256-bin histogram needs its own `with` module (there is no `serde_big_array` or similar
/// dependency pinned for this crate, and the dependency policy asks not to add one for this).
/// Serializes and deserializes a `[u64; 256]` as a plain 256-element JSON array.
mod bins {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S>(value: &[u64; 256], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.as_slice().serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<[u64; 256], D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = Vec::<u64>::deserialize(deserializer)?;
        let length = values.len();
        values.try_into().map_err(|_: Vec<u64>| {
            serde::de::Error::custom(format!("expected 256 histogram bins, found {length}"))
        })
    }
}

/// An exact reduction of one rendered raster: three 256-bin per-channel histograms and the output
/// endpoint (clipping) counters the histogram and clipping contract defines. Every channel's bins
/// sum to `width * height`. `domain` is always `"srgb-8bit-output"`: the rendered SDR sRGB output,
/// never an inference about RAW/sensor clipping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    #[serde(with = "bins")]
    pub r: [u64; 256],
    #[serde(with = "bins")]
    pub g: [u64; 256],
    #[serde(with = "bins")]
    pub b: [u64; 256],
    /// Pixels whose red channel is exactly code 0.
    pub r0: u64,
    /// Pixels whose green channel is exactly code 0.
    pub g0: u64,
    /// Pixels whose blue channel is exactly code 0.
    pub b0: u64,
    /// Pixels whose red channel is exactly code 255.
    pub r255: u64,
    /// Pixels whose green channel is exactly code 255.
    pub g255: u64,
    /// Pixels whose blue channel is exactly code 255.
    pub b255: u64,
    /// Pixels with at least one channel at code 0.
    pub any_shadow: u64,
    /// Pixels with at least one channel at code 255.
    pub any_highlight: u64,
    /// Pixels whose red, green and blue channels are all code 0.
    pub all_shadow: u64,
    /// Pixels whose red, green and blue channels are all code 255.
    pub all_highlight: u64,
    /// Pixels that are simultaneously `any_shadow` and `any_highlight`.
    pub both: u64,
    pub width: u32,
    pub height: u32,
    #[serde(deserialize_with = "deserialize_domain")]
    pub domain: &'static str,
}

impl Report {
    /// The output pixel count the reduction covers, recovered from the red channel's bin sum: any
    /// channel's sum equals `width * height` under the histogram and clipping contract.
    pub fn pixel_count(&self) -> u64 {
        self.r.iter().sum()
    }
}

/// A pixel's output clipping class: which endpoint(s) it has a channel at. This is the one
/// predicate the per-channel/any/all counters below, the future clipping overlays and the future
/// API share, so every consumer of "is this pixel clipped" agrees byte for byte. Alpha is never
/// consulted: the supported JPEG path is opaque, and clipping describes color channels only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Clip {
    /// At least one channel is code 0, none is code 255.
    Shadow,
    /// At least one channel is code 255, none is code 0.
    Highlight,
    /// At least one channel is code 0 and at least one (possibly the same, for a single-channel
    /// endpoint pixel that is also both bounds on other channels) is code 255.
    Both,
}

/// The shared clipping predicate: `None` when no channel of `rgba` sits at an output endpoint,
/// otherwise which endpoint(s) it has. Alpha (`rgba[3]`) is ignored.
pub(crate) fn clip_class(rgba: [u8; 4]) -> Option<Clip> {
    let shadow = rgba[0] == 0 || rgba[1] == 0 || rgba[2] == 0;
    let highlight = rgba[0] == 255 || rgba[1] == 255 || rgba[2] == 255;
    match (shadow, highlight) {
        (true, true) => Some(Clip::Both),
        (true, false) => Some(Clip::Shadow),
        (false, true) => Some(Clip::Highlight),
        (false, false) => None,
    }
}

/// Which endpoints one channel value sits at, as bits: bit 0 for code 0, bit 1 for code 255.
/// `EXTREME[r] | EXTREME[g] | EXTREME[b]` is then the pixel's any-channel class and `&` its
/// all-channel class (0 none, 1 shadow, 2 highlight, 3 both), so one lookup per channel and two
/// counter increments replace the per-channel comparisons and the `clip_class` match. This is the
/// same predicate [`clip_class`] states, held to it by the exhaustive test below.
const EXTREME: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut value = 0;
    while value < 256 {
        table[value] = (value == 0) as u8 | (((value == 255) as u8) << 1);
        value += 1;
    }
    table
};

/// The pixels one worker chunk of the parallel reduction covers, and the span the serial path
/// checks the token over. Counting is integer addition, so the split decides nothing about the
/// result: the same buffer reduces to the same bins whatever the chunking is.
const REDUCE_CHUNK_PIXELS: usize = 64 * 1024;

// A chunk counts in `u32`. Every counter below rises by at most one per pixel, so none can exceed
// the chunk's pixel count; this is the bound that makes the plain `u32` addition safe.
const _: () = assert!(REDUCE_CHUNK_PIXELS as u64 <= u32::MAX as u64);

/// One chunk's counts, in `u32` and on the stack: three 256-bin histograms and the two four-way
/// class counts. It lives only while one chunk is counted and is then widened into [`Bins`].
struct ChunkCounts {
    r: [u32; 256],
    g: [u32; 256],
    b: [u32; 256],
    /// Pixels by `EXTREME[r] | EXTREME[g] | EXTREME[b]`.
    any: [u32; 4],
    /// Pixels by `EXTREME[r] & EXTREME[g] & EXTREME[b]`.
    all: [u32; 4],
}

impl ChunkCounts {
    /// Count the pixels of `chunk`: tightly packed RGBA, at most [`REDUCE_CHUNK_PIXELS`] of them.
    /// Alpha is never consulted. The caller has already checked the buffer length, so no partial
    /// trailing pixel exists.
    #[inline]
    fn count(chunk: &[u8]) -> Self {
        debug_assert!(chunk.len() <= REDUCE_CHUNK_PIXELS * 4);
        let mut counts = Self {
            r: [0; 256],
            g: [0; 256],
            b: [0; 256],
            any: [0; 4],
            all: [0; 4],
        };
        for pixel in chunk.chunks_exact(4) {
            let (r, g, b) = (
                usize::from(pixel[0]),
                usize::from(pixel[1]),
                usize::from(pixel[2]),
            );
            counts.r[r] += 1;
            counts.g[g] += 1;
            counts.b[b] += 1;
            let (er, eg, eb) = (EXTREME[r], EXTREME[g], EXTREME[b]);
            counts.any[usize::from(er | eg | eb) & 3] += 1;
            counts.all[usize::from(er & eg & eb) & 3] += 1;
        }
        counts
    }
}

/// Worker-local accumulator: three 256-bin histograms plus the two four-way class counts, sized
/// once per worker (not per pixel or per image), widened in from each chunk's [`ChunkCounts`] and
/// merged into other workers' bins by addition. The per-channel endpoint counters and the any, all
/// and both counters of the [`Report`] are derived from these in `into_report`.
#[derive(Clone)]
struct Bins {
    r: [u64; 256],
    g: [u64; 256],
    b: [u64; 256],
    any: [u64; 4],
    all: [u64; 4],
}

impl Bins {
    fn zero() -> Self {
        Self {
            r: [0; 256],
            g: [0; 256],
            b: [0; 256],
            any: [0; 4],
            all: [0; 4],
        }
    }

    /// Count one chunk and widen its counts into these bins. 128 MP (the largest source-size
    /// limit) is comfortably below `u64::MAX`, so the accumulator adds with plain addition and
    /// never needs checked or saturating arithmetic; see
    /// `counts_cannot_overflow_within_the_raw_pixel_limit` below.
    fn add_chunk(&mut self, chunk: &[u8]) {
        let counts = ChunkCounts::count(chunk);
        for i in 0..256 {
            self.r[i] += u64::from(counts.r[i]);
            self.g[i] += u64::from(counts.g[i]);
            self.b[i] += u64::from(counts.b[i]);
        }
        for i in 0..4 {
            self.any[i] += u64::from(counts.any[i]);
            self.all[i] += u64::from(counts.all[i]);
        }
    }

    /// Add `other`'s counts into `self` in place. Used instead of a by-value merge so the
    /// parallel path below never moves a whole ~6 KiB `Bins` through Rayon's recursive
    /// fold/reduce combinators: in an unoptimized (debug) build that recursion carries every
    /// intermediate value on the stack, and a struct this size at typical photo-sized split
    /// depths was observed to overflow a worker thread's default stack. Merging through a
    /// `Box<Bins>` (a pointer-sized move at every recursion level) avoids that regardless of
    /// build profile.
    fn merge_in_place(&mut self, other: &Self) {
        for i in 0..256 {
            self.r[i] += other.r[i];
            self.g[i] += other.g[i];
            self.b[i] += other.b[i];
        }
        for i in 0..4 {
            self.any[i] += other.any[i];
            self.all[i] += other.all[i];
        }
    }

    /// The report's counters from the bins. A channel's code-0 and code-255 pixels are exactly its
    /// first and last bin. In a class count bit 0 is a shadow endpoint and bit 1 a highlight
    /// endpoint, so a shadow (highlight) pixel is one in class 1 or 3 (2 or 3) and class 3 holds
    /// the pixels with both. A pixel cannot have all three channels at both endpoints, so
    /// `all[3]` is always zero; it is added all the same to keep the two counts symmetric.
    fn into_report(self, width: u32, height: u32) -> Report {
        let [_, any_shadow_only, any_highlight_only, both] = self.any;
        let [_, all_shadow_only, all_highlight_only, all_both] = self.all;
        Report {
            r0: self.r[0],
            g0: self.g[0],
            b0: self.b[0],
            r255: self.r[255],
            g255: self.g[255],
            b255: self.b[255],
            any_shadow: any_shadow_only + both,
            any_highlight: any_highlight_only + both,
            all_shadow: all_shadow_only + all_both,
            all_highlight: all_highlight_only + all_both,
            both,
            r: self.r,
            g: self.g,
            b: self.b,
            width,
            height,
            domain: DOMAIN,
        }
    }
}

fn reduce_serial(rgba: &[u8], cancel: &Cancel) -> Result<Bins, Error> {
    let mut bins = Bins::zero();
    // One relaxed load per chunk, not per pixel; the counting loop is the chunk's own.
    for chunk in rgba.chunks(REDUCE_CHUNK_PIXELS * 4) {
        cancel.check()?;
        bins.add_chunk(chunk);
    }
    Ok(bins)
}

fn reduce_parallel(rgba: &[u8], cancel: &Cancel) -> Result<Bins, Error> {
    // Accumulators are boxed (see `merge_in_place`'s doc comment) so every fold/reduce step moves
    // a pointer, not a ~6 KiB `Bins`. The token is read once per worker chunk, before that chunk's
    // pixels are counted.
    let boxed = rgba
        .par_chunks(REDUCE_CHUNK_PIXELS * 4)
        .try_fold(
            || Box::new(Bins::zero()),
            |mut bins, chunk| {
                cancel.check()?;
                bins.add_chunk(chunk);
                Ok::<_, Error>(bins)
            },
        )
        .try_reduce(
            || Box::new(Bins::zero()),
            |mut a, b| {
                a.merge_in_place(&b);
                Ok(a)
            },
        )?;
    Ok(*boxed)
}

/// [`reduce`] with an explicit parallel threshold: the hook the tests use to force either path on
/// the same buffer. Production code always goes through [`reduce`], which fixes the threshold at
/// [`luxforge_raw::PARALLEL_PIXELS`], the one the passes that are not rendering passes share.
fn reduce_with_threshold(
    rgba: &[u8],
    width: u32,
    height: u32,
    threshold: u64,
    cancel: &Cancel,
) -> Result<Report, Error> {
    // A token already cancelled when the call arrives counts nothing at all.
    cancel.check()?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::resource_limit("image dimensions overflow"))?;
    let byte_len = pixels
        .checked_mul(4)
        .ok_or_else(|| Error::resource_limit("image dimensions overflow"))?;
    let byte_len = usize::try_from(byte_len)
        .map_err(|_| Error::resource_limit("image allocation is not addressable"))?;
    if rgba.len() != byte_len {
        return Err(Error::validation(format!(
            "pixel buffer holds {} bytes, expected {byte_len} for a {width}x{height} image",
            rgba.len()
        )));
    }
    let bins = if pixels >= threshold {
        reduce_parallel(rgba, cancel)?
    } else {
        reduce_serial(rgba, cancel)?
    };
    Ok(bins.into_report(width, height))
}

/// Reduce one immutable byte raster (tightly packed RGBA, row-major) into an exact [`Report`]: a
/// rendered [`crate::Raster`]'s `rgba`, `width` and `height`, or any buffer of that layout.
/// Reduces serially below the one-megapixel threshold [`luxforge_raw::PARALLEL_PIXELS`],
/// and on the shared Rayon pool above it, using bounded worker-local bins merged by addition. Reads
/// `rgba` in place: no copy of the raster and no allocation proportional to the image (`Bins` is a
/// fixed handful of kilobytes per worker, not per pixel). `cancel` is read once per worker chunk,
/// so the exact phase's reduction stops within one chunk of a newer request; a caller with nothing
/// to supersede passes [`Cancel::never`].
pub fn reduce(rgba: &[u8], width: u32, height: u32, cancel: &Cancel) -> Result<Report, Error> {
    reduce_with_threshold(rgba, width, height, luxforge_raw::PARALLEL_PIXELS, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Raster;
    use std::{collections::HashMap, fs, path::PathBuf};

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic")
    }

    #[derive(Deserialize)]
    struct FixtureCounts {
        #[serde(with = "bins")]
        r: [u64; 256],
        #[serde(with = "bins")]
        g: [u64; 256],
        #[serde(with = "bins")]
        b: [u64; 256],
    }

    #[derive(Deserialize)]
    struct FixtureClipping {
        r0: u64,
        g0: u64,
        b0: u64,
        r255: u64,
        g255: u64,
        b255: u64,
        any_shadow: u64,
        any_highlight: u64,
        all_shadow: u64,
        all_highlight: u64,
        both: u64,
    }

    #[derive(Deserialize)]
    struct Fixture {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        counts: FixtureCounts,
        clipping: FixtureClipping,
    }

    /// Every `fixtures/basic/*.json` file shaped like a histogram fixture: an object with both
    /// `rgba` and `counts` keys. `exposure-cases.json` (a bare JSON array) and `mixed-order.json`
    /// (an object with `base_rgba`, no `counts`) are skipped, matching the README's "the ones with
    /// `rgba`" and its "a loader should ignore unknown fields" note.
    fn load_histogram_fixtures() -> Vec<(String, Fixture)> {
        let mut out = Vec::new();
        for entry in fs::read_dir(fixtures_dir()).expect("fixtures/basic exists") {
            let path = entry.expect("readable fixtures/basic entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("readable fixture file");
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("fixture file is valid JSON");
            let Some(object) = value.as_object() else {
                continue;
            };
            if !object.contains_key("rgba") || !object.contains_key("counts") {
                continue;
            }
            let fixture: Fixture =
                serde_json::from_value(value).expect("histogram fixture matches the schema");
            let name = path
                .file_name()
                .expect("fixture path has a file name")
                .to_string_lossy()
                .into_owned();
            out.push((name, fixture));
        }
        assert!(
            !out.is_empty(),
            "expected at least one histogram fixture under fixtures/basic"
        );
        out
    }

    #[test]
    fn hand_counted_fixtures_match_exactly() {
        for (name, fixture) in load_histogram_fixtures() {
            let report = reduce(
                &fixture.rgba,
                fixture.width,
                fixture.height,
                &Cancel::never(),
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(report.r, fixture.counts.r, "{name}: r channel");
            assert_eq!(report.g, fixture.counts.g, "{name}: g channel");
            assert_eq!(report.b, fixture.counts.b, "{name}: b channel");
            assert_eq!(report.r0, fixture.clipping.r0, "{name}: r0");
            assert_eq!(report.g0, fixture.clipping.g0, "{name}: g0");
            assert_eq!(report.b0, fixture.clipping.b0, "{name}: b0");
            assert_eq!(report.r255, fixture.clipping.r255, "{name}: r255");
            assert_eq!(report.g255, fixture.clipping.g255, "{name}: g255");
            assert_eq!(report.b255, fixture.clipping.b255, "{name}: b255");
            assert_eq!(
                report.any_shadow, fixture.clipping.any_shadow,
                "{name}: any_shadow"
            );
            assert_eq!(
                report.any_highlight, fixture.clipping.any_highlight,
                "{name}: any_highlight"
            );
            assert_eq!(
                report.all_shadow, fixture.clipping.all_shadow,
                "{name}: all_shadow"
            );
            assert_eq!(
                report.all_highlight, fixture.clipping.all_highlight,
                "{name}: all_highlight"
            );
            assert_eq!(report.both, fixture.clipping.both, "{name}: both");
            assert_eq!(report.width, fixture.width, "{name}: width");
            assert_eq!(report.height, fixture.height, "{name}: height");
            assert_eq!(report.domain, DOMAIN, "{name}: domain");

            let pixels = u64::from(fixture.width) * u64::from(fixture.height);
            for (label, channel) in [("r", &report.r), ("g", &report.g), ("b", &report.b)] {
                let sum: u64 = channel.iter().sum();
                assert_eq!(sum, pixels, "{name}: {label} channel sum");
            }
            assert_eq!(report.pixel_count(), pixels, "{name}: pixel_count");
        }
    }

    #[test]
    fn cropped_population_pair_matches_the_border_contribution() {
        let fixtures: HashMap<String, Fixture> = load_histogram_fixtures().into_iter().collect();
        let full = &fixtures["cropped-population-full-6x4.json"];
        let crop = &fixtures["cropped-population-crop-4x2.json"];
        let full_report = reduce(&full.rgba, full.width, full.height, &Cancel::never()).unwrap();
        let crop_report = reduce(&crop.rgba, crop.width, crop.height, &Cancel::never()).unwrap();

        // The README's hand check: the full composition's border ring is 12 black pixels (code 0)
        // and 4 white pixels (code 255); the crop excludes the whole ring, so subtracting the
        // crop's per-bin counts from the full composition's leaves exactly that border-only
        // population on every channel, and nothing elsewhere.
        for channel in 0..256usize {
            let expected = match channel {
                0 => 12,
                255 => 4,
                _ => 0,
            };
            assert_eq!(
                full_report.r[channel] - crop_report.r[channel],
                expected,
                "r[{channel}]"
            );
            assert_eq!(
                full_report.g[channel] - crop_report.g[channel],
                expected,
                "g[{channel}]"
            );
            assert_eq!(
                full_report.b[channel] - crop_report.b[channel],
                expected,
                "b[{channel}]"
            );
        }
        assert_eq!(full_report.r0 - crop_report.r0, 12);
        assert_eq!(full_report.g0 - crop_report.g0, 12);
        assert_eq!(full_report.b0 - crop_report.b0, 12);
        assert_eq!(full_report.r255 - crop_report.r255, 4);
        assert_eq!(full_report.g255 - crop_report.g255, 4);
        assert_eq!(full_report.b255 - crop_report.b255, 4);
        assert_eq!(full_report.any_shadow - crop_report.any_shadow, 12);
        assert_eq!(full_report.any_highlight - crop_report.any_highlight, 4);
        assert_eq!(full_report.all_shadow - crop_report.all_shadow, 12);
        assert_eq!(full_report.all_highlight - crop_report.all_highlight, 4);
        assert_eq!(full_report.both - crop_report.both, 0);
    }

    /// A deterministic buffer with no clipped pixels and no special alignment: wide coverage of the
    /// 256 bins without landing back on a hand-counted fixture.
    fn synthetic_buffer(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let r = (x.wrapping_mul(7).wrapping_add(y.wrapping_mul(13)) % 256) as u8;
                let g = (x.wrapping_mul(3).wrapping_add(y.wrapping_mul(251)) % 256) as u8;
                let b = (x.wrapping_mul(97) ^ y.wrapping_mul(31)) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        rgba
    }

    #[test]
    fn serial_and_parallel_reductions_agree_exactly() {
        // The threshold hook forces either path at any size. 512x384 is exactly three worker
        // chunks; 509x389 is three and a remainder, exercising remainder handling in a parallel
        // split.
        for (width, height) in [(512u32, 384u32), (509u32, 389u32)] {
            let rgba = synthetic_buffer(width, height);
            let serial = reduce_with_threshold(&rgba, width, height, u64::MAX, &Cancel::never())
                .expect("serial reduction of the synthetic buffer");
            let parallel = reduce_with_threshold(&rgba, width, height, 0, &Cancel::never())
                .expect("parallel reduction of the synthetic buffer");
            assert_eq!(
                serial, parallel,
                "{width}x{height}: serial vs. forced-parallel"
            );
        }
    }

    /// The counting arithmetic as it was before the class table: one `add_pixel` per pixel into
    /// `u64` counters, a separate counter for every channel endpoint, and `clip_class` for the any
    /// and both counters. It is frozen here as the independent reference the production reducer is
    /// compared with, so it must not be edited to follow the production code.
    struct ReferenceBins {
        r: [u64; 256],
        g: [u64; 256],
        b: [u64; 256],
        r0: u64,
        g0: u64,
        b0: u64,
        r255: u64,
        g255: u64,
        b255: u64,
        any_shadow: u64,
        any_highlight: u64,
        all_shadow: u64,
        all_highlight: u64,
        both: u64,
    }

    impl ReferenceBins {
        fn add_pixel(&mut self, pixel: [u8; 4]) {
            let [r, g, b, _alpha] = pixel;
            self.r[r as usize] += 1;
            self.g[g as usize] += 1;
            self.b[b as usize] += 1;
            let (r0, g0, b0) = (r == 0, g == 0, b == 0);
            let (r255, g255, b255) = (r == 255, g == 255, b == 255);
            self.r0 += u64::from(r0);
            self.g0 += u64::from(g0);
            self.b0 += u64::from(b0);
            self.r255 += u64::from(r255);
            self.g255 += u64::from(g255);
            self.b255 += u64::from(b255);
            self.all_shadow += u64::from(r0 && g0 && b0);
            self.all_highlight += u64::from(r255 && g255 && b255);
            match clip_class(pixel) {
                Some(Clip::Shadow) => self.any_shadow += 1,
                Some(Clip::Highlight) => self.any_highlight += 1,
                Some(Clip::Both) => {
                    self.any_shadow += 1;
                    self.any_highlight += 1;
                    self.both += 1;
                }
                None => {}
            }
        }
    }

    /// The report the frozen pre-change arithmetic gives for `rgba`, one pixel at a time.
    fn reference_report(rgba: &[u8], width: u32, height: u32) -> Report {
        let mut bins = ReferenceBins {
            r: [0; 256],
            g: [0; 256],
            b: [0; 256],
            r0: 0,
            g0: 0,
            b0: 0,
            r255: 0,
            g255: 0,
            b255: 0,
            any_shadow: 0,
            any_highlight: 0,
            all_shadow: 0,
            all_highlight: 0,
            both: 0,
        };
        for pixel in rgba.chunks_exact(4) {
            bins.add_pixel([pixel[0], pixel[1], pixel[2], pixel[3]]);
        }
        Report {
            r: bins.r,
            g: bins.g,
            b: bins.b,
            r0: bins.r0,
            g0: bins.g0,
            b0: bins.b0,
            r255: bins.r255,
            g255: bins.g255,
            b255: bins.b255,
            any_shadow: bins.any_shadow,
            any_highlight: bins.any_highlight,
            all_shadow: bins.all_shadow,
            all_highlight: bins.all_highlight,
            both: bins.both,
            width,
            height,
            domain: DOMAIN,
        }
    }

    /// Reduce `rgba` through both the serial and the forced-parallel path and require each whole
    /// [`Report`] to equal the frozen reference's.
    fn assert_matches_reference(rgba: &[u8], width: u32, height: u32, label: &str) {
        let reference = reference_report(rgba, width, height);
        for (path, threshold) in [("serial", u64::MAX), ("parallel", 0)] {
            let report = reduce_with_threshold(rgba, width, height, threshold, &Cancel::never())
                .unwrap_or_else(|error| panic!("{label} {path}: {error}"));
            assert_eq!(report, reference, "{label}: {path} reduction vs. reference");
        }
    }

    #[test]
    fn every_rgb_triple_reduces_exactly_as_the_frozen_reference() {
        // The whole 8-bit RGB domain, one pixel per (r, g, b) triple: 2^24 pixels, 4096 x 4096,
        // 256 worker chunks in the parallel path. Alpha varies and is never consulted. The whole
        // `Report` is compared, so a one-count difference in any bin or counter fails.
        let pixels: usize = 1 << 24;
        let mut rgba = Vec::with_capacity((pixels + 2 * 65536) * 4);
        for i in 0..pixels as u32 {
            rgba.extend_from_slice(&[
                (i >> 16) as u8,
                (i >> 8) as u8,
                i as u8,
                (i ^ (i >> 11)) as u8,
            ]);
        }
        assert_matches_reference(&rgba, 4096, 4096, "every triple");

        // The same counts in closed form, so the reference itself is checked and not only the
        // production reducer against it.
        let report = reduce_with_threshold(&rgba, 4096, 4096, 0, &Cancel::never())
            .expect("parallel reduction of every triple");
        let total: u64 = 256 * 256 * 256;
        let without_zero: u64 = 255 * 255 * 255;
        let without_either: u64 = 254 * 254 * 254;
        assert_eq!(report.r0, 256 * 256);
        assert_eq!(report.b255, 256 * 256);
        assert_eq!(report.any_shadow, total - without_zero);
        assert_eq!(report.any_highlight, total - without_zero);
        assert_eq!(report.both, total + without_either - 2 * without_zero);
        assert_eq!(report.all_shadow, 1);
        assert_eq!(report.all_highlight, 1);

        // The cube is symmetric: it counts a value 254 at an endpoint exactly as it counts 255,
        // and shadow exactly as highlight. Repeating every triple with red at 255 and then every
        // triple with green at 0 breaks that symmetry, so a misplaced endpoint or a swapped class
        // cannot agree with the reference.
        for i in 0..65536u32 {
            rgba.extend_from_slice(&[255, (i >> 8) as u8, i as u8, 0]);
        }
        for i in 0..65536u32 {
            rgba.extend_from_slice(&[(i >> 8) as u8, 0, i as u8, 255]);
        }
        assert_eq!(rgba.len(), 4096 * 4128 * 4);
        assert_matches_reference(&rgba, 4096, 4128, "every triple, skewed");
    }

    #[test]
    fn chunk_edges_and_clip_heavy_noise_reduce_exactly_as_the_frozen_reference() {
        // Buffers whose length straddles the worker chunk (one pixel, one short of a chunk, a
        // chunk, one over, and two chunks and a remainder), each uniform at an endpoint pair and
        // then a seeded noise buffer with a quarter of its channel bytes at 0 and a quarter at 255.
        let chunk = REDUCE_CHUNK_PIXELS;
        let uniform: [[u8; 4]; 5] = [
            [0, 0, 0, 255],
            [255, 255, 255, 0],
            [0, 255, 0, 7],
            [255, 128, 255, 9],
            [1, 254, 128, 255],
        ];
        for pixel in uniform {
            for count in [1, chunk - 1, chunk, chunk + 1, 2 * chunk + 17] {
                let rgba: Vec<u8> = pixel.iter().copied().cycle().take(count * 4).collect();
                assert_matches_reference(&rgba, count as u32, 1, &format!("{pixel:?} x {count}"));
            }
        }

        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let count = 3 * chunk + 123;
        let rgba: Vec<u8> = (0..count * 4)
            .map(|_| {
                let value = next();
                match value & 3 {
                    0 => 0,
                    1 => 255,
                    _ => (value >> 8) as u8,
                }
            })
            .collect();
        assert_matches_reference(&rgba, count as u32, 1, "seeded clip-heavy noise");
    }

    #[test]
    fn invalid_length_and_overflow_are_structured_errors() {
        // A 4x1 image needs 16 bytes; give it 15.
        let short = vec![0u8; 15];
        let error =
            reduce(&short, 4, 1, &Cancel::never()).expect_err("a short buffer must be rejected");
        assert_eq!(error.kind, ErrorKind::Validation);

        let error = reduce(&[], u32::MAX, u32::MAX, &Cancel::never())
            .expect_err("overflowing dimensions must be rejected");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
    }

    #[test]
    fn counts_cannot_overflow_within_the_raw_pixel_limit() {
        // The largest admitted source-size limit is 128 MP. Every counter in `Bins` accumulates with plain
        // (unchecked) addition; this states and proves the bound that makes that safe: even a
        // report whose every pixel hits the same bin and every predicate stays far below
        // `u64::MAX`, so no counter can wrap or need checked arithmetic.
        const MAX_PIXELS: u64 = luxforge_raw::MAX_PIXELS as u64;
        const _: () = assert!(
            MAX_PIXELS < u64::MAX / 4,
            "128 MP must stay far below u64::MAX even summed across several counters"
        );
    }

    #[test]
    fn clip_class_table() {
        assert_eq!(clip_class([0, 0, 0, 255]), Some(Clip::Shadow));
        assert_eq!(clip_class([255, 255, 255, 255]), Some(Clip::Highlight));
        assert_eq!(clip_class([0, 255, 0, 255]), Some(Clip::Both));
        assert_eq!(clip_class([1, 254, 7, 255]), None);
        // Alpha is ignored: varying it changes nothing.
        assert_eq!(clip_class([0, 0, 0, 0]), Some(Clip::Shadow));
        assert_eq!(clip_class([255, 255, 255, 128]), Some(Clip::Highlight));
        assert_eq!(clip_class([0, 255, 0, 0]), Some(Clip::Both));
        assert_eq!(clip_class([1, 254, 7, 0]), None);
    }

    #[test]
    fn report_serializes_within_the_16_kib_bound() {
        // A representative worst case: every counter at the 128 MP source-size limit and maximal
        // dimensions, so every field's JSON digit count is near its practical maximum.
        const MAX_PIXELS: u64 = luxforge_raw::MAX_PIXELS as u64;
        let report = Report {
            r: [MAX_PIXELS; 256],
            g: [MAX_PIXELS; 256],
            b: [MAX_PIXELS; 256],
            r0: MAX_PIXELS,
            g0: MAX_PIXELS,
            b0: MAX_PIXELS,
            r255: MAX_PIXELS,
            g255: MAX_PIXELS,
            b255: MAX_PIXELS,
            any_shadow: MAX_PIXELS,
            any_highlight: MAX_PIXELS,
            all_shadow: MAX_PIXELS,
            all_highlight: MAX_PIXELS,
            both: MAX_PIXELS,
            width: 16384,
            height: 16384,
            domain: DOMAIN,
        };
        let json = serde_json::to_vec(&report).expect("a Report always serializes");
        assert!(
            json.len() <= REPORT_BOUND_BYTES,
            "serialized report is {} bytes, over the {REPORT_BOUND_BYTES}-byte contract bound",
            json.len()
        );
        println!(
            "measured report size: {} bytes (bound {REPORT_BOUND_BYTES})",
            json.len()
        );
    }

    // -------------------------------------------------------------------------------------------
    // Cooperative cancellation.
    // -------------------------------------------------------------------------------------------

    fn cancellation_raster(width: u32, height: u32) -> Raster {
        Raster {
            width,
            height,
            rgba: synthetic_buffer(width, height).into(),
            source_fingerprint: "sha256:reducer-cancellation".into(),
            snapshot_id: crate::SnapshotId::new(),
        }
    }

    #[test]
    fn a_pre_cancelled_token_stops_the_reducer_on_either_path() {
        let raster = cancellation_raster(512, 384);
        let cancel = Cancel::new();
        cancel.cancel();
        for threshold in [u64::MAX, 0] {
            let error = reduce_with_threshold(
                &raster.rgba,
                raster.width,
                raster.height,
                threshold,
                &cancel,
            )
            .expect_err("a cancelled token refuses the reduction");
            assert_eq!(error.kind, ErrorKind::Cancelled);
            assert_eq!(error.kind.code(), "cancelled");
        }
    }
}
