# RawSpeed unpacking

Status: authorized by the owner on 2026-09-30; implementation in progress ([plan](../../tasks/raw-cameras.json)).

For the recording modes where it is exact and faster, the RAW adapter fills the sensor mosaic with [RawSpeed](https://github.com/darktable-org/rawspeed) instead of LibRaw's own decoder. LibRaw still identifies the file, reads every piece of metadata, and runs every step of its unpack after the pixels are read. The [RawSpeed evaluation](../research/rawspeed-evaluation.md) measured 2.5–3.7× faster unpacking on Nikon lossless and lossy NEF, Canon CR2, Fujifilm lossless compressed RAF, ORF, PEF and lossless and packed DNG, with byte-identical samples and metadata. Those families are about a third of popular-camera files and save 130–550 ms per open. CR3 and Sony ARW stay on LibRaw.

## Behavior

- The retained u16 mosaic of a routed mode is byte-identical to LibRaw's, and every metadata field the adapter reads is identical. Development, rendering, history and export are unchanged.
- Routing is a per-mode catalog capability: a mode's optional `unpacker` is `libraw` (the default when omitted) or `rawspeed`. There is no runtime choice, environment switch or automatic retry. If RawSpeed fails on a routed mode, the open fails with a RAW native error naming RawSpeed; it never falls back to LibRaw's decoder.
- A routed mode is enabled only with authentic evidence that its mosaic hash equals the LibRaw-only hash recorded in [the evidence manifest](../../fixtures/modern-camera-evidence.json).

## Design

**Classify before unpack.** The native open currently identifies and unpacks in one call, and Rust classifies the mode afterwards. The call splits: `open` runs LibRaw's `open_buffer` and returns the identify-time metadata (make, model, decoder name, raw size, bit depth, CFA, frame count, DNG version). Rust classifies the mode from that metadata and the source bytes, and then calls `unpack` with the mode's unpacker. After unpack, the adapter checks that the geometry and decoder are unchanged, as it does today. Unsupported modes are then refused before any unpack work. The High Efficiency refusal in [popular camera support](popular-camera-support.md#behavior) stays ahead of both calls.

**Substituting LibRaw's decoder.** The adapter's LibRaw subclass sets LibRaw's `load_raw` member to its own RawSpeed decode after `open_buffer` and before `unpack`. LibRaw's `unpack` then allocates its raw buffer, calls that decode, and runs `crop_masked_pixels`, black statistics and every later step unchanged. The decode:

1. Wraps the caller's borrowed encoded bytes in a RawSpeed `Buffer` without copying.
2. Parses them with `RawParser`, and configures the decoder with `uncorrectedRawValues = true`, `applyCrop = false`, `interpolateBadPixels = false`, `applyStage1DngOpcodes = false` and `failOnUnknown = true`. It then calls `checkSupport` and `decodeRaw`.
3. Requires a one-component u16 image whose uncropped size equals LibRaw's `raw_width` × `raw_height`.
4. Writes each row into LibRaw's `raw_image` at `raw_pitch`, through LibRaw's own `curve` table when the replaced LibRaw decoder applies that curve, and unchanged when it does not.

Any RawSpeed exception becomes a LibRaw decode failure, which the adapter reports as an explicit error. No exception crosses the C ABI.

**The replaceable decoders are code-owned.** The curve rule is a property of the LibRaw decoder being replaced, so it belongs to one table in the adapter, keyed by LibRaw decoder name: `nikon_load_raw()`, `lossless_jpeg_load_raw()`, `fuji_compressed_load_raw()`, `olympus_load_raw()`, `pentax_load_raw()`, `lossless_dng_load_raw()`, `packed_dng_load_raw()`, `panasonic_load_raw()`, `panasonicC6_load_raw()` and `panasonicC8_load_raw()`. The catalog build refuses `rawspeed` on a mode whose decoder is not in the table. Adding a decoder to the table needs the exactness evidence below across its corpus.

**Camera data.** RawSpeed needs its `cameras.xml` for support checks and decoder hints. The pinned file is embedded in the binary and parsed once per process, on the first routed unpack, into one immutable RawSpeed `CameraMetaData`. The parse is guarded by a one-time initializer and takes about 4 ms, on the source worker. Nothing reads it from disk or the environment. RawSpeed's white, black, crop and colour values in that file are never used.

**Build.** RawSpeed is vendored at commit `c835b05aecfacb7343f7c424abd620aa12116c3f` (the tip of `develop` on 2026-09-30). Only the library sources, `data/cameras.xml` and the licences are vendored, with pugixml at a pinned release. `build.rs` compiles RawSpeed as C++20 in its own `cc` build, separate from LibRaw's C++17 flags. The build is generic-CPU, with no OpenMP, no zlib and no libjpeg: deflate and lossy DNG are unsupported and never routed. RawSpeed's CMake-generated configuration headers are written by hand. No source patch is applied to RawSpeed or LibRaw. [THIRD_PARTY.md](../../crates/luxforge-raw/THIRD_PARTY.md) records provenance, the LGPL-2.0-or-later code licence and the CC-BY-SA 3.0 `cameras.xml` licence. The macOS build is required. Linux and Windows builds keep going through the existing portability checks; native Windows (MSVC or clang-cl) is unverified and recorded as such.

## Constraints and consequences

- **Liveness.** RawSpeed's `decodeRaw` has no progress callback. Cancellation is checked immediately before and after it. The longest routed decode measured is 305 ms (Fujifilm X-T5 lossless compressed), which replaces 858 ms of LibRaw work that could be cancelled part-way. A cancelled open still publishes nothing.
- **Memory.** No encoded copy is made. RawSpeed decodes into its own image, which is released as soon as the rows are copied, so a routed unpack briefly holds a second u16 mosaic (for example 91 MiB at 45.7 MP). The retained mosaic, the development and every later buffer are unchanged.
- **Threading.** Serial, on the existing source worker. No OpenMP, thread pool or new executor.
- **Attack surface.** Both parsers read a routed file. RawSpeed is fuzzed upstream; the adapter's existing size bounds apply before either runs.

## Evidence and acceptance

- Unit tests: the catalog refuses `rawspeed` on an unlisted decoder and on unknown values. Classification before unpack refuses unsupported modes without unpacking. A RawSpeed failure on a routed mode is an explicit error, with no LibRaw fallback. Cancellation before and after the RawSpeed decode publishes nothing.
- Authentic tests for every routed mode: the routed mosaic hash equals the LibRaw-only hash in the evidence manifest; the source hash is unchanged; the metadata equals the LibRaw-only metadata; and development is finite. The owner Z6 and Air 2S originals and the existing oracles (`bayer_owner_mosaic_and_rgb_oracle`, the DNG reference) still pass.
- The initial routed set is every catalog mode on a replaceable decoder whose evidence passes and whose unpack is at least 1.3× faster in the adapter's own release measurement. A mode that fails either stays on LibRaw, and the reason is recorded in [modern camera support](modern-camera-support.md).
- Measurement, after the feature is complete:
  - adapter unpack time per routed mode, before and after;
  - the owner Z6's cold saved-WB preparation p50 and p95, against the 546 ms baseline in [performance](../specs/performance.md#native-development-and-saved-white-balance-preparation);
  - peak process RSS for one open of the largest routed mode.
  They follow the [performance rules](../engineering/performance-rules.md) and are recorded in the [performance spec](../specs/performance.md).
