//! The standalone exact RGB histogram reducer and output-clipping predicates.
//!
//! This is a pure function over an already-rendered byte raster: it needs no host, job or module
//! change and creates neither a recipe effect nor a history entry (see the [histogram and clipping
//! contract](../../../docs/design/basic-and-histogram.md#histogram-and-clipping-contract) and the
//! integration contract's "Analysis jobs and identity"). It reduces the **rendered SDR sRGB
//! output** of a composition, not scene-linear or RAW/sensor data.

use crate::{Error, ErrorKind, Raster};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Mirrors `render::PARALLEL_RENDER_PIXELS`: the same one-megapixel threshold the rasterizer uses
/// for its own parallel pass. Kept as a separate constant because `render.rs` is out of scope for
/// this reducer; the two must be changed together if the measured threshold ever moves.
const PARALLEL_REDUCE_PIXELS: u64 = 1_000_000;

/// The histogram and clipping contract's declared output domain: the rendered SDR sRGB output of
/// the full current composition, after crop and edits, before UI overlays or display scaling. Not
/// the camera/RAW histogram.
const DOMAIN: &str = "srgb-8bit-output";

/// A report is bounded to 16 KiB of counters/metadata before protocol encoding (the histogram and
/// clipping contract, and "Resource and responsiveness constraints").
pub const REPORT_BOUND_BYTES: usize = 16 * 1024;

fn deserialize_domain<'de, D>(deserializer: D) -> Result<&'static str, D::Error>
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

    pub fn serialize<S>(value: &[u64; 256], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u64; 256], D::Error>
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
pub enum Clip {
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
pub fn clip_class(rgba: [u8; 4]) -> Option<Clip> {
    let shadow = rgba[0] == 0 || rgba[1] == 0 || rgba[2] == 0;
    let highlight = rgba[0] == 255 || rgba[1] == 255 || rgba[2] == 255;
    match (shadow, highlight) {
        (true, true) => Some(Clip::Both),
        (true, false) => Some(Clip::Shadow),
        (false, true) => Some(Clip::Highlight),
        (false, false) => None,
    }
}

/// Worker-local accumulator: three 256-bin histograms plus the endpoint counters, sized once per
/// worker (not per pixel or per image), merged into other workers' bins by addition.
#[derive(Clone)]
struct Bins {
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

impl Bins {
    fn zero() -> Self {
        Self {
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
        }
    }

    /// Fold one pixel into these bins. 64 MP (the current source-size limit) is comfortably below
    /// `u64::MAX`, so every counter here accumulates with plain addition and never needs checked or
    /// saturating arithmetic; see `counts_cannot_overflow_within_the_64_megapixel_limit` below.
    #[inline]
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
        self.r0 += other.r0;
        self.g0 += other.g0;
        self.b0 += other.b0;
        self.r255 += other.r255;
        self.g255 += other.g255;
        self.b255 += other.b255;
        self.any_shadow += other.any_shadow;
        self.any_highlight += other.any_highlight;
        self.all_shadow += other.all_shadow;
        self.all_highlight += other.all_highlight;
        self.both += other.both;
    }

    fn into_report(self, width: u32, height: u32) -> Report {
        Report {
            r: self.r,
            g: self.g,
            b: self.b,
            r0: self.r0,
            g0: self.g0,
            b0: self.b0,
            r255: self.r255,
            g255: self.g255,
            b255: self.b255,
            any_shadow: self.any_shadow,
            any_highlight: self.any_highlight,
            all_shadow: self.all_shadow,
            all_highlight: self.all_highlight,
            both: self.both,
            width,
            height,
            domain: DOMAIN,
        }
    }
}

fn reduce_serial(rgba: &[u8]) -> Bins {
    let mut bins = Bins::zero();
    for pixel in rgba.chunks_exact(4) {
        bins.add_pixel([pixel[0], pixel[1], pixel[2], pixel[3]]);
    }
    bins
}

fn reduce_parallel(rgba: &[u8]) -> Bins {
    // Accumulators are boxed (see `merge_in_place`'s doc comment) so every fold/reduce step moves
    // a pointer, not a ~6 KiB `Bins`.
    let boxed = rgba
        .par_chunks_exact(4)
        .fold(
            || Box::new(Bins::zero()),
            |mut bins, pixel| {
                bins.add_pixel([pixel[0], pixel[1], pixel[2], pixel[3]]);
                bins
            },
        )
        .reduce(
            || Box::new(Bins::zero()),
            |mut a, b| {
                a.merge_in_place(&b);
                a
            },
        );
    *boxed
}

/// `reduce` with an explicit parallel threshold, so tests can force either path on the same
/// buffer. Production code always goes through `reduce`, which fixes the threshold at
/// `PARALLEL_REDUCE_PIXELS`.
fn reduce_with_threshold(
    rgba: &[u8],
    width: u32,
    height: u32,
    threshold: u64,
) -> Result<Report, Error> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::new(ErrorKind::ResourceLimit, "image dimensions overflow"))?;
    let byte_len = pixels
        .checked_mul(4)
        .ok_or_else(|| Error::new(ErrorKind::ResourceLimit, "image dimensions overflow"))?;
    let byte_len = usize::try_from(byte_len).map_err(|_| {
        Error::new(
            ErrorKind::ResourceLimit,
            "image allocation is not addressable",
        )
    })?;
    if rgba.len() != byte_len {
        return Err(Error::new(
            ErrorKind::Validation,
            format!(
                "pixel buffer holds {} bytes, expected {byte_len} for a {width}x{height} image",
                rgba.len()
            ),
        ));
    }
    let bins = if pixels >= threshold {
        reduce_parallel(rgba)
    } else {
        reduce_serial(rgba)
    };
    Ok(bins.into_report(width, height))
}

/// Reduce one immutable byte raster (tightly packed RGBA, row-major) into an exact [`Report`].
/// Reduces serially below the one-megapixel threshold `render.rs` uses for its own parallel pass,
/// and on the shared Rayon pool above it, using bounded worker-local bins merged by addition. Reads
/// `rgba` in place: no copy of the raster and no allocation proportional to the image (`Bins` is a
/// fixed handful of kilobytes per worker, not per pixel).
pub fn reduce(rgba: &[u8], width: u32, height: u32) -> Result<Report, Error> {
    reduce_with_threshold(rgba, width, height, PARALLEL_REDUCE_PIXELS)
}

/// [`reduce`] over a rendered [`Raster`], for callers that already hold one (a preview job, a
/// desktop analysis request, `editor-performance`).
pub fn reduce_raster(raster: &Raster) -> Result<Report, Error> {
    reduce(raster.rgba.as_ref(), raster.width, raster.height)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            let report = reduce(&fixture.rgba, fixture.width, fixture.height)
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
        let full_report = reduce(&full.rgba, full.width, full.height).unwrap();
        let crop_report = reduce(&crop.rgba, crop.width, crop.height).unwrap();

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
        // 2000x600 is comfortably over the one-megapixel threshold and a round multiple of common
        // chunk sizes; 1013x977 is just under a megapixel and not a multiple of any convenient
        // chunk size, exercising remainder handling in a parallel split.
        for (width, height) in [(2000u32, 600u32), (1013u32, 977u32)] {
            let rgba = synthetic_buffer(width, height);
            let serial = reduce_with_threshold(&rgba, width, height, u64::MAX)
                .expect("serial reduction of the synthetic buffer");
            let parallel = reduce_with_threshold(&rgba, width, height, 0)
                .expect("parallel reduction of the synthetic buffer");
            assert_eq!(
                serial, parallel,
                "{width}x{height}: serial vs. forced-parallel"
            );
        }
    }

    #[test]
    fn invalid_length_and_overflow_are_structured_errors() {
        // A 4x1 image needs 16 bytes; give it 15.
        let short = vec![0u8; 15];
        let error = reduce(&short, 4, 1).expect_err("a short buffer must be rejected");
        assert_eq!(error.kind, ErrorKind::Validation);

        let error =
            reduce(&[], u32::MAX, u32::MAX).expect_err("overflowing dimensions must be rejected");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
    }

    #[test]
    fn counts_cannot_overflow_within_the_64_megapixel_limit() {
        // The current source-size limit is 64 MP. Every counter in `Bins` accumulates with plain
        // (unchecked) addition; this states and proves the bound that makes that safe: even a
        // report whose every pixel hits the same bin and every predicate stays far below
        // `u64::MAX`, so no counter can wrap or need checked arithmetic.
        const MAX_PIXELS: u64 = 64 * 1024 * 1024;
        const _: () = assert!(
            MAX_PIXELS < u64::MAX / 4,
            "64 MP must stay far below u64::MAX even summed across several counters"
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
        // A representative worst case: every counter at the 64 MP source-size limit and maximal
        // dimensions, so every field's JSON digit count is near its practical maximum.
        const MAX_PIXELS: u64 = 64 * 1024 * 1024;
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
}
