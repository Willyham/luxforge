# Luxforge RAW adapter

`luxforge-raw` is one of the few Luxforge crates with an explicit unsafe FFI boundary ([architecture](../../docs/design/architecture.md#unsafe-code)). It builds pinned native source locally; the rest of the workspace keeps its `forbid(unsafe_code)` rule. The safe API takes source bytes owned by the caller, unpacks one qualified RAW image on a worker, retains one immutable sensor mosaic, and develops green-normalized white-balance edits (positive gains up to 32×) into one owned planar float allocation. It also lists and extracts the embedded previews of any RAW file LibRaw identifies, reading the file by position and never unpacking it ([embedded previews](#embedded-previews)). It never rewrites an original or creates an intermediate file. [THIRD_PARTY.md](THIRD_PARTY.md) records provenance, notices, build flags, and the exact native source selection.

The pinned [RawSpeed](../../docs/design/rawspeed-unpack.md) and its pugixml are also built, unpatched, as C++20 in their own `cc` build, with RawSpeed's `cameras.xml` embedded and parsed once per process on first use. A catalog mode whose `unpacker` is `rawspeed` has its mosaic filled by RawSpeed inside LibRaw's unpack, in place of a replaceable LibRaw decoder; LibRaw still identifies the file and supplies all metadata. 64 catalog modes are routed ([modern camera support](../../docs/design/modern-camera-support.md#rawspeed-routed-modes)); every other mode keeps LibRaw's decoder. `RawMetadata::backend` names what filled the mosaic: `LibRaw 0.22.2 + RawSpeed c835b05a + librtprocess 9a858270` for a routed mode, `LibRaw 0.22.2 + librtprocess 9a858270` otherwise. RawSpeed's log messages go to standard error, never standard output.

```rust
let source = RawSource::decode(verified_bytes, &cancel)?;
let metadata = source.metadata();
let camera_linear = source.develop(metadata.as_shot_gains, &cancel)?;
// camera_linear.data is [red plane, green plane, blue plane] in sensor order.
```

The caller must read/hash a stable original through the catalog's source-verification path before passing its bytes, which `decode` takes as any owned or borrowed buffer and never copies. This crate is a CPU worker operation and does not own catalog identity, history, scheduling, display conversion, or GPU upload. `decode` borrows the encoded bytes through LibRaw `open_buffer`, classifies the recording mode, unpacks, copies its u16 mosaic once into a Rust `Vec`, closes LibRaw, and drops the encoded bytes. The returned `RawSource` uses `Arc<Vec<u16>>`; clones share the sensor allocation. `develop` subtracts LibRaw scalar/channel/repeating black, normalizes each CFA sample by `sensor_white - black` to librtprocess's 65535 sensor scale and applies per-channel WB before demosaic, as `((sample - black) * scale) * gain` in `f32`; runs RCD for Bayer or one-pass Markesteijn for X-Trans through the native adapter; and divides the planar result by 65535 in Rust. Rust computes the per-site black, scale and gain once per site of the CFA and black-repeat period, and the demosaic computes each site's value as it reads it from the retained mosaic, through the staged `librtprocess-mosaic.patch`, so no float mosaic exists. A development whose sensor stage rewrites the normalized values (a DNG stage-one radial vignette or stage-two gain maps, or sparse bad-pixel repairs) instead normalizes into a float mosaic in Rust on the development executor, corrects it there and demosaics that; the two inputs give the same bits for the same values. Each development takes one input, chosen before it starts. The neutral picker reads the same per-site black model. Neither `develop` nor the DNG corrections scan the planes for finiteness; the caller checks every value once after its last arithmetic, as the core's camera conversion does before adopting the planes. The adapter clamps nothing, but the pinned RCD clips each gained Bayer site to [0, 65536/65535] of sensor white before interpolating (outside its 9 px border band) and reconstructs non-negative values; Markesteijn does neither, so X-Trans keeps over-white and negative values ([pixel and color contract](../../docs/design/initial-raw.md#pixel-and-color-contract)). A WB change reruns from the immutable mosaic. Exposure, camera→working matrix and later edits can use the developed planes without rereading the source.

`RawMetadata` records the validated full sensor shape, active area, camera default crop, raw LibRaw inset, CFA phase and 2×2/6×6 pattern, black pattern, sensor white, as-shot green-normalized gains, EXIF orientation 1–8, and color matrices. `rgb_cam` is LibRaw's WB-balanced camera RGB→linear sRGB matrix; the caller applies it once after `develop` and must not apply `pre_mul` again. For non-DNG modes, `cam_xyz` is LibRaw's calibration unless the profile supplies a source-attributed fixed matrix. Configured matrices use the pinned LibRaw coefficient conversion; missing or singular XYZ-to-camera calibration fails explicitly. For DNG modes the adapter validates and exposes the profile-selected source `ColorMatrix` as the XYZ→camera matrix, with both source matrix hashes and illuminants in `dng_corrections`. This supports the existing fixed-matrix WB controls; it does not interpolate between illuminants. Current Fuji framing uses RAF camera crop tags; LibRaw's inset trims three additional top rows and is exposed separately. Exact mode validation matches configured sensor dimensions, decoder, bits, CFA dimensions and raw frame count. Container validation additionally checks explicit compression markers where selected, including Z6 lossless maker-note marker 3 and X100VI RAF header marker 0/2. Before LibRaw opens a file, `decode` refuses Nikon High Efficiency for every model, catalogued or not, as `RawError::UnsupportedCompression` ("unsupported RAW compression: Nikon High Efficiency (HE/HE*) is not supported; record Lossless compressed RAW instead"; the core's `unsupported-input`): a Nikon maker note recording NEF compression 13 or 14, or a Nikon file whose largest image IFD's first strip or tile begins with the JPEG XS markers `FF10 FF50`. The check reads the maker note and four bytes at that offset through the bounded TIFF reader; structure it cannot follow is left to the decoder, never refused. LibRaw 0.22.2 recognizes High Efficiency only on the Z 8, Z 9, Z f and Z6_3 and reads the Z50_2 and Z5_2 files as lossless, so this container check is the one that catches them. As a second check, the adapter refuses a file for which LibRaw selects `nikon_he_load_raw()` after open, before unpack. DNG modes validate storage, calibration, crop and the configured opcode layout. The primary image is selected from a declared one- or two-frame container; secondary Dual Pixel data is not blended. Extension or camera-name acceptance alone is insufficient.

### Native open, classification and unpack

The native decode is two calls on one handle, which a Rust guard owns and closes on every return path:

1. `lf_raw_open` runs LibRaw's `open_buffer` (identify only), refuses LibRaw's High Efficiency decoder, a camera outside the catalog allowlist, a frame count outside one or two and a size outside the adapter bounds, and returns the identify-time identity: make, model, decoder name and flags, raw width and height, `raw_bps`, DNG version, frame count, and the CFA dimensions and pattern.
2. Rust bounds the size, reads and checks the DNG opcode lists, and classifies the mode from that identity and the container's compression marker. An unknown mode, an unimplemented required opcode or an opcode the mode's profile does not handle is refused here, before any unpack work.
3. `lf_raw_unpack` unpacks once with the mode's catalog `unpacker`: LibRaw's own decoder (`NativeUnpacker::Libraw`, 0) or RawSpeed in its place (`NativeUnpacker::Rawspeed`, 1, below); any other selector is refused before unpack. It checks that the geometry and stride are unchanged and that the result is one integer mosaic, then fills the complete metadata (black levels, white, matrices, crops). Nothing is published on failure or cancellation.

#### RawSpeed in place of LibRaw's decoder

For `NativeUnpacker::Rawspeed` the identified LibRaw decoder must be in the replaceable table, `REPLACEABLE` in [`src/unpacker.rs`](src/unpacker.rs), which the build script also writes into the adapter's generated `rawspeed_decoders.h`; the catalog build refuses `rawspeed` on any other decoder. The row's guard and the shared preconditions (a one-channel CFA mosaic, no Fujifilm SuperCCD rotation) must hold. Otherwise the selector is refused before unpack and the handle stays unpackable. The adapter's LibRaw subclass then sets LibRaw's protected `load_raw` to its own method and calls LibRaw's `unpack`, which allocates the raw buffer, calls the method where it would call its decoder, and runs `crop_masked_pixels`, the black adjustment and every later step unchanged. The method first restores the replaced decoder, because those steps compare `load_raw` with LibRaw's decoders. It decodes the handle's borrowed encoded bytes with RawSpeed (no copy; uncorrected values, no crop, no bad-pixel interpolation, no stage-1 DNG opcodes, unknown cameras refused, `checkSupport`, `decodeRaw`), refuses any error RawSpeed recorded, requires a one-component u16 image of exactly `raw_width` × `raw_height`, writes each row at `raw_pitch` through the replaced decoder's curve rule, and releases RawSpeed's image before returning. Any RawSpeed failure fails the unpack with a `RawError::Native` message that starts `RawSpeed:` and carries RawSpeed's text; LibRaw's decoder is never tried instead. Cancellation is checked immediately before and after `decodeRaw`, which cannot be interrupted.
4. Rust compares every identity field with the unpacked metadata and fails explicitly, naming the field, if LibRaw changed any of them.

In the pinned LibRaw 0.22.2 every field classification reads is final at identify for every catalogued decoder (`nikon_load_raw`, `nikon_14bit_load_raw`, `unpacked_load_raw`, `fuji_compressed_load_raw`, `packed_dng_load_raw`, `lossless_dng_load_raw`, `lossless_jpeg_load_raw`, `pentax_load_raw`, `panasonic_load_raw`, `panasonicC6_load_raw`, `panasonicC8_load_raw`, `sony_arw2_load_raw`, `olympus_load_raw`, `crxLoadRaw`). Make, model, frame count, DNG version, the decoder and `color.raw_bps` are written only by identify. `unpack` rewrites the raw size and margins only on its legacy four-colour path and in RawSpeed builds, neither of which a catalogued mode reaches. No catalogued decoder, and not `crop_masked_pixels`, writes `filters`, `xtrans_abs` or the margins the CFA phase reads; the decoders read `tiff_bps` but never write it or `raw_bps`. Unpack does rewrite the white level (`crxLoadRaw`, and `unpacked_load_raw` temporarily) and the black levels (`crop_masked_pixels`), which classification does not read and which are taken only after unpack. The step 4 comparison therefore re-validates rather than repairs: it keeps a classified mode honest if a future LibRaw or decoder changes one of these fields.

The qualified DJI Air 2S FC3411 DNG has required OpcodeList3 GainMap (9) followed by per-channel WarpRectilinear (1). `decode` associates them with the unique raw sensor SubIFD, validates their versions, area, finite values and warp geometry, and rejects unknown mandatory operations. `develop` applies the gain in active-area coordinates and then resamples each camera plane through its own chromatic warp before the caller's color matrix and default crop. It recognizes the exact identity green warp and retains those gained pixels without interpolation. It reuses one active-plane scratch buffer and keeps the full-sensor output layout. Crate-private bounded per-channel queries map a corrected point to its sensor location and gain for the neutral picker. `dng_corrections` records the exact operation order, payload hashes, optional operations skipped and interpretation identity. This float path preserves negative values and highlight headroom. The DNG specification calls for clipping after OpcodeList2/3, so these float values are not a strict clipped DNG rendering when an intermediate value leaves [0,1]. This preserves the existing Luxforge RAW contract: scene headroom remains available to later exposure edits, with clipping only at terminal display.

The public metadata includes validated `active_area`, `default_crop`, `black_cfa`,
CFA and calibration fields. The selected mode declares the validated raw frame count. The current catalog contains 107
camera profiles and 126 recording modes; resource and visible-quality
qualification remains ongoing.

The ordered DNG path also recognizes FixVignetteRadial (3),
FixBadPixelsConstant (4), and FixBadPixelsList (5). Bad-pixel operations produce
bounded sparse replacements from an immutable source mosaic; unresolved points
remain unchanged and are reported. Vignette evaluation follows DNG normalized
outer-pixel coordinates; this implementation records its interpretation rather
than claiming blanket SDK bit identity.

## Embedded previews

`EmbeddedPreviews` lists the images a RAW file carries beside its mosaic and extracts one, for browsing: it never unpacks the mosaic (by LibRaw or RawSpeed) and never consults the camera catalog, so it serves any RAW LibRaw identifies, from any camera. `RawSource::decode` and its catalog and mode checks are separate and unchanged.

```rust
let file = std::fs::File::open(path)?;
let mut previews = EmbeddedPreviews::open(file, 16 << 20, &cancel)?; // read budget
// Make, model, visible and raw size, orientation and every embedded image.
let largest = previews.listing().largest_jpeg().map(|preview| preview.index);
if let Some(index) = largest {
    let image = previews.extract(index, 16 << 20, &cancel)?; // EmbeddedImage::Jpeg(bytes)
}
println!("{} bytes read", previews.bytes_read());
```

**Reading by position.** LibRaw reads the source through a datastream of the adapter's own, in [native/embedded.cpp](native/embedded.cpp) (a sibling of the decode adapter, compiled into the same native library), whose every fetch calls back into Rust for a positional read of the caller's `RandomAccess` source: a `File` (`read_at` on Unix, `seek_read` on Windows), bytes in memory, or a reference, `Box` or `Arc` of either. Small reads come from a cache of eight 4 KiB-aligned 16 KiB blocks, the least recently read replaced first, so identify's many 1–4-byte reads cost one fetch per block; a read of at least a block goes straight to its destination in pieces of at most 1 MiB. Every call answers as LibRaw's own buffer datastream (the one `open_buffer` uses) does, which a test drives both with the same random calls to prove; `gets` follows `fgets`, where the buffer stream writes its terminator one byte late at the end of the data. Every file of the [inventory](../../docs/research/embedded-previews.md) lists exactly what the buffer stream lists for the whole file in memory. Listing read 17 to 205 KB a file there, 121 KB on average.

**Listing.** After identify the handle lists LibRaw's thumbnail list (at most eight images) with each image's LibRaw index, format, listed dimensions and stored length, and the item's own orientation where LibRaw read one, beside LibRaw's make and model, the visible and raw sizes and the file's EXIF orientation. LibRaw writes `FF D8` over the first two bytes of every JPEG it extracts, and lists Canon's H.265 previews and any TIFF preview whose compression it does not name as JPEGs, so the handle reads each listed JPEG's first eight stored bytes: SOI makes a `Jpeg`, Canon's H.265 header an `H265`, anything else `NotJpeg`. Listed dimensions cannot rank previews (LibRaw lists a Canon CR3's full-size JPEG as 0 × 0); `largest_jpeg` ranks by stored length.

**Extraction.** `extract(index, max_bytes, cancel)` runs LibRaw's `unpack_thumb_ex` and returns owned bytes: a JPEG exactly as stored, checked to begin with SOI, or 8-bit RGB. It hands over JPEGs and 8-bit bitmaps (LibRaw's layer and PPM kinds, one or three channels, a one-channel bitmap repeated into three), Rollei's 5-6-5 bitmap and 16-bit bitmaps, which LibRaw converts to 8 bits itself by keeping each sample's high byte (the adapter never sets `LIBRAW_RAWOPTIONS_USE_PPM16_THUMBS`). H.265, JPEG XL, a listed JPEG without SOI, LibRaw's Kodak kinds (which also take any uncompressed TIFF preview of more than 8 bits, such as Canon CR2's 16-bit RGB image, and which LibRaw decodes through a small development), DNG YCbCr, X3F and bitmaps LibRaw would misread are refused as `RawError::UnsupportedCompression` naming the format. Before LibRaw allocates, the adapter refuses an image whose stored bytes run past the end of the source, or whose LibRaw allocation would pass `max_bytes`: the stored length for a JPEG or PPM, twice the pixels for a layer bitmap, five bytes a pixel for Rollei, three times the samples for a 16-bit bitmap. It checks again what LibRaw produced. LibRaw's buffer and the Rust copy exist together, so one extraction holds at most twice `max_bytes`; LibRaw's buffer is freed, through its own allocator, before `extract` returns.

**Failure and cancellation.** Each fetch checks the cancel token, and LibRaw's progress callback checks it too; both are set for one synchronous call and cleared when it returns. A reader's error or panic is caught in the callback, never unwinding into C++, and reported: an I/O error, or a source shorter than its length at open, as `RawError::Io` with the error's kind, so a caller can tell an unplugged card from a corrupt file, and a panic as `RawError::Native` (`source read: …`); running past the read budget is `RawError::ResourceLimit`, checked before the bytes are read. A stopped read throws inside LibRaw, which may discard its state, so after a cancellation or failure while reading the handle refuses further extractions: open the source again. A refused index, format or byte limit leaves it usable.

**Ownership and threads.** Rust owns the reader and the native handle: the native stream points at the reader, which the handle frees only after closing the native handle, exactly once, on every path. One handle serves one source on one thread at a time (every native call takes `&mut self`); it is `Send` when its reader is, and never `Sync`. Each handle holds about 1.2 MB of native memory: LibRaw's object (750 KiB), its per-object scratch (277 KiB) and the 128 KiB read cache.

| Bound | Figure |
| --- | --- |
| One extracted image, LibRaw's buffer and the copy each | The caller's `max_bytes`, at most `MAX_EMBEDDED_IMAGE_BYTES` (64 MiB) |
| Bytes a handle reads from its source over its life | The caller's read budget, at most `MAX_EMBEDDED_READ_BUDGET` (128 MiB) |
| A handle's read cache | 8 blocks of 16 KiB |

## Camera catalog

[data/cameras.json](data/cameras.json) is the single source for Luxforge camera
identities, sensor dimensions, CFA dimensions, recording-mode selectors, crop
source and DNG calibration/correction settings. The [format contract](../../docs/design/raw-camera-profiles.md)
describes the fields and how to add a profile. The strict parser in
[src/profiles.rs](src/profiles.rs) validates the catalog at build time; the build
produces the native pre-unpack allowlist and the catalog itself as static Rust
data from it, so nothing parses JSON at run time. A test proves the static data
equals the parsed JSON. `RawMode` is a reference into that catalog: it serializes
as the mode's identifier, and only a declared identifier deserializes.
Adding a camera that uses existing capabilities requires a data entry and authentic
qualification, without a new camera-name branch. Unknown models/modes and invalid
capability combinations fail explicitly.

Camera-dependent processing chooses typed capabilities (`raf_tags`, `dng_tags`,
`root_fixed_matrix`, `stage3_gain_map_then_warp`, `stage_ordered`), never make/model or mode labels.
Capture-specific calibration, crops and optical coefficients still come from the
original. TIFF/RAF/NEF encoding constants, numerical algorithms, resource bounds,
and pinned LibRaw's upstream camera tables remain code-owned.

## Bounds and liveness

These are RAW adapter limits. JPEG rendering keeps its separate existing
RGBA8/frame and scratch limits; the RAW increase does not change JPEG behavior.
The 512 MiB encoded, 128 MP sensor, and 1.5 GiB per-planar-buffer values are
approved implementation limits. One authentic selected mode per model has
adapter qualification; editor measurements are recorded in the resource ledger.
The 1.5 GiB value is not a process RSS budget.

| Allocation or resource | Bound / owner |
| --- | --- |
| Encoded source | 512 MiB RAW-only Rust input limit; borrowed for the synchronous native decode only |
| Sensor shape | 16,384 per side and 128 million pixels, checked before Rust allocation and after native unpack; primary frame only |
| Native unpack | LibRaw `max_raw_memory_mb=512`, plus Rust checked dimensions/stride; native temporary decoder closes before source publication. A RawSpeed-routed unpack also holds RawSpeed's own u16 image of the same sensor size until its rows are copied into LibRaw's buffer, and a 128 KiB value map for the Nikon curve rule |
| Retained u16 sensor mosaic | One Rust allocation, `2 × pixels`, shared by `Arc<Vec<u16>>` |
| Developed float output | One Rust `Vec<f32>` of `3 × pixels`, capped at 1.5 GiB per planar RGB buffer; `[R plane,G plane,B plane]` |
| Demosaic input | The retained u16 mosaic, read through three per-site tables of one CFA and black-repeat period, each row at least 64 sites (1,536 bytes for a 2×2 Bayer period, 4,752 for a 6×6 X-Trans period). A development whose sensor stage rewrites the normalized values (DNG stage-one vignette or stage-two gain maps, or sparse repairs) holds one temporary float mosaic, `4 × pixels`, instead. Plus algorithm tile workspace and row-pointer tables; released before `develop` returns |
| Demosaic tile scratch | One native heap allocation per admitted callback, 988,208 bytes for X-Trans Markesteijn and 978,536 for Bayer RCD, at most eight process-wide (7,905,664 bytes total), plus callback stack arrays outside that heap count. The shared Rayon pool runs bounded ordinary tile jobs; the final job, which carries the frame's last tile rows, runs on the source caller. No private thread pool or full-frame scratch copy. |
| DNG stage-three warp | One temporary active-area float plane, `4 × active pixels`, reused for three channels after native demosaic scratch is released |
| Job concurrency | Owned by the editor worker; reserve for in-flight mosaic/output/old presentation before starting replacement |

At full X100VI sensor size (7872×5196 = 40,902,912 pixels), retained u16 mosaic is about 78 MiB and the developed three-plane output about 468 MiB, before native decoder scratch, worker overlap, color/geometry and GPU storage; a development that needs a float mosaic holds about 156 MiB more while it demosaics. The adapter's per-buffer checks do not by themselves enforce the editor's combined-process memory target. The caller must bound active/pending workers and account for old visible results during replacement.

Cancellation is checked before decode, through LibRaw's synchronous progress callback during identify and unpack, immediately before and after unpack, once the per-site tables are built, before every row of a float mosaic, before every RCD or Markesteijn tile, after librtprocess returns, and every 64 rows during each DNG gain/warp pass. Both demosaics run bounded tile jobs with process-wide scratch admission; each job is a contiguous run of the serial tile raster that begins at a full tile, so partial edge tiles keep the scratch history they read. X-Trans dimensions below 120 px and Bayer dimensions below 10 px on either side fail explicitly. See [the execution and lifetime contract](../../docs/design/native-demosaic-parallelism.md). No C++ exception crosses the ABI. A Rust `AtomicBool` is accessed only by an `extern "C"` callback during a synchronous native call; it is never reinterpreted as a C++ atomic or retained beyond the call.

## Evidence and limits

`cargo test -p luxforge-raw --locked` checks malformed container bounds, required/optional opcode flags, cycles, cancellation before work, and geometry. On synthetic DNGs it checks the split native flow: unsupported cameras and modes are refused with no unpack started (a per-thread native counter), unknown unpacker selectors are refused, cancellation at every callback of identify and unpack publishes no metadata, every error path releases the handle, and a changed identity field fails. Local authentic tests run with:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw \
LUXFORGE_RAW_PUBLIC_DIR=/path/to/cc0/raw \
LUXFORGE_RAW_POPULAR_DIR=/path/to/popular/raw \
  cargo test --release -p luxforge-raw --locked --test real_files -- --ignored --nocapture
```

The complete Markesteijn serial/pool oracle, including changed white balance, edge geometry, concurrent callers and native fault recovery, runs separately:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw \
  cargo test --release -p luxforge-raw --locked --lib markesteijn_parallel_complete_float_oracle -- --ignored --nocapture
```

The Bayer oracle checks the serial RGB planes against an immutable pre-change M4 digest and compares complete normalized mosaics and RGB planes from the serial raster with pooled RCD at one, two, four and the default number of workers, and with changed white balance, one source per process (`nikon_z6.NEF` or `mavic_air_2s.DNG`):

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_PROFILE_SOURCE=nikon_z6.NEF \
  cargo test --release -p luxforge-raw --locked --lib bayer_owner_mosaic_and_rgb_oracle -- --ignored --nocapture
```

The Nikon High Efficiency refusal runs against the SHA-verified raw.pixls.us samples named `<id>.<EXT>`: the six High Efficiency files (Z 9, Z 8, Z f, Z6_3, Z50_2, Z5_2) are refused, lossless controls from the same bodies (Z 9, Z f, Z 6II, Z6_3, Z50_2) decode to the mosaics in [the evidence manifest](../../fixtures/modern-camera-evidence.json), and a library test opens the same files directly to check the adapter's own refusal of the four files LibRaw routes to its High Efficiency decoder (the Z50_2 and Z5_2 files, which LibRaw misreads as lossless, pass the native open and are refused only by the container check) and that no other sample in the directory is refused:

```sh
LUXFORGE_RAW_POPULAR_DIR=/path/to/popular/raw \
  cargo test --release -p luxforge-raw --locked --test real_files nikon_high_efficiency -- --ignored --nocapture
LUXFORGE_RAW_POPULAR_DIR=/path/to/popular/raw \
  cargo test --release -p luxforge-raw --locked --lib native_open_refuses -- --ignored --nocapture
```

The owner and public corpus checks are separate tests. If only owner originals are available, run the same command with `--skip authentic_public_modes --skip nikon_high_efficiency` and report the public modes as untested.

The unit tests also route synthetic DNGs through RawSpeed: the routed mosaic and every native metadata field equal LibRaw's, with and without a LinearizationTable that LibRaw's DNG decoder applies as its curve; the selector is refused before unpack for a decoder outside the table and for a failed guard; a RawSpeed failure (a DNG version RawSpeed refuses, and an injected decoder exception) is an explicit error with no LibRaw fallback; cancellation at every callback of a routed unpack publishes nothing, with two consecutive checks around `decodeRaw`; and a RawSpeed warning reaches standard error and nothing reaches standard output.

The RawSpeed comparisons need authentic files. The first decodes the owner Z6 NEF and, from the CC0 popular-camera corpus (files named `<raw.pixls.us id>.<EXT>`), a Nikon Z 6II lossy NEF (`4161.NEF`) and a Canon EOS 6D Mark II CR2 (`1625.CR2`) with both libraries, and requires LibRaw's `curve[RawSpeed uncorrected sample]` to equal LibRaw's sample everywhere, and the CR2's uncorrected samples to equal LibRaw's directly. The second routes every replaceable decoder through RawSpeed on the owner Z6 and Air 2S originals and on popular and corpus samples, and requires LibRaw's mosaic and native metadata exactly, the LibRaw-only mosaic hash where the [evidence manifest](../../fixtures/modern-camera-evidence.json) or the owner test records one, and, for a catalogued mode, the identical `RawSource` mosaic, every metadata field but `backend` identical, and a finite development; it also checks that an injected RawSpeed failure on a routed decode is an explicit error:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_POPULAR_DIR=/path/to/popular \
  LUXFORGE_RAW_CORPUS_DIR=/path/to/corpus \
  cargo test --release -p luxforge-raw --locked --lib rawspeed -- --ignored --nocapture \
  --skip replaceable_catalog_modes
```

The third is the routing evidence. It decodes every RAW file in the directories `LUXFORGE_RAW_SAMPLE_DIRS` lists (a path list; each file once, whatever its name) and, for every one in a catalog mode whose decoder is in the replaceable table, decodes it with both unpackers through `RawSource::decode_forcing`. It requires the identical mosaic, every metadata field but `backend` identical, identical as-shot and perturbed-white-balance developments, LibRaw's results equal to those recorded before routing (the [evidence manifest](../../fixtures/modern-camera-evidence.json)'s mosaic and development hashes, or the mosaic pins of `tests/real_files.rs` for the modes without an evidence entry), and the catalog's own decode to name the mode's unpacker. It prints a row per candidate mode and fails unless every routed mode is exact on every sample found, with every evidence sample of it found:

```sh
LUXFORGE_RAW_SAMPLE_DIRS=/path/to/selection:/path/to/popular:/path/to/corpus:/path/to/owner/raw \
  cargo test --release -p luxforge-raw --locked --lib replaceable_catalog_modes -- --ignored --nocapture
```

Two ignored measurements go with it, and run alone in release. `rawspeed_unpack_timing` reads the same directories and times `RawSource::decode` with LibRaw forced against RawSpeed for every routed mode (or the modes `LUXFORGE_RAW_TIMING_MODES` lists), writing every observation to the CSV `LUXFORGE_RAW_TIMING_OUTPUT` names and printing one JSON line per mode with each sample's p50, p95 and ratio; this is the routing speed gate's measurement ([performance](../../docs/specs/performance.md#rawspeed-unpacking)). `one_decode_for_peak_rss` decodes `LUXFORGE_RAW_FIXTURE` once with the unpacker `LUXFORGE_RAW_UNPACKER` names (`libraw` or `rawspeed`), for a peak RSS read from outside the process:

```sh
LUXFORGE_RAW_SAMPLE_DIRS=/path/to/selection:/path/to/popular:/path/to/corpus:/path/to/owner/raw \
  LUXFORGE_RAW_TIMING_OUTPUT=/path/to/unpack.csv cargo test --release -p luxforge-raw --locked \
  --lib rawspeed_unpack_timing -- --ignored --nocapture
```

The sensor-site input's evidence reads the same directories. Every file the catalog decodes is developed at its as-shot gains and at the qualifier's perturbed white balance twice, through the input its development takes and through a forced float mosaic, and the planes must be identical bit for bit. It prints a row per file: its layout, its input, each outcome with the bytes the development holds through the demosaic either way, and whether the complete development (DNG corrections after the demosaic included) keeps the [evidence manifest](../../fixtures/modern-camera-evidence.json)'s hash where that records the source. A file the catalog refuses is listed, not failed. The unit tests hold the sensor sites to the float mosaic on synthetic Bayer and X-Trans mosaics in every CFA phase, at the tile edges and past sensor white, with the same cancellation, refusals and fault handling, and show which DNG corrections keep the float mosaic:

```sh
LUXFORGE_RAW_SAMPLE_DIRS=/path/to/selection:/path/to/popular:/path/to/owner/raw \
  cargo test --release -p luxforge-raw --locked --lib sensor_sites_match_the_float_mosaic \
  -- --ignored --nocapture
```

The authentic tests compare full sensor u16 buffers to the hashes the [RAW backend comparison](../../docs/research/raw-backend-selection.md) recorded independently, verify source hashes before/after, mode, crop, CFA, white metadata, owner Z6 EXIF orientation, finite developed floats, the developed float range (printed), invalid gains and cancellation. The DJI test also checks the opcode/calibration payload hashes, fixed matrix against LibRaw's rendered matrix, malformed mandatory operations, and 18 corrected camera-plane samples computed independently from sparse pre-correction pixels in [the DNG reference](../../fixtures/raw-dng-reference.json) (`luxforge_reference::dng`). Its crate-private required-opcode list and corrected point queries are checked against the same file by the ignored library tests `owner_dji_dng_requires_warp_and_gain_map` and `owner_dji_dng_answers_corrected_point_queries` (`--lib owner_dji -- --ignored`). The fixture itself and generated sparse dump stay outside the repository. Manual dependency/native/asset review and clean Windows/Linux package verification are still outstanding. This crate alone does not qualify visible color, export or end-to-end latency.

The embedded-preview unit tests (`cargo test -p luxforge-raw --locked --lib embedded`) build a DNG laid out as cameras write one, a JPEG preview in IFD0 and the mosaic in a SubIFD, and check the listing and the extracted bytes exactly, reading 17.6 KB of its 1.6 MB; that a file and the same bytes in memory list and extract alike; typed refusals, with no handle left open, for empty, garbage and every truncated prefix of the DNG; cancellation before work and at each of identify's and an extraction's fetches; the read budget at and one byte below what identify needs; the byte limit and index checked before any read; a reader's error, panic and early end; H.265 and non-JPEG bytes behind a listed JPEG; `largest_jpeg`'s ranking; and the native stream against LibRaw's buffer datastream over four sequences of 20,000 random calls across block boundaries and the end of the data. The authentic checks only read their files. The first extracts every image of the owner's three originals, decodes each JPEG with `image`, checks that `largest_jpeg` is the largest, that listing and extracting it read under half the file, and that each file is unchanged; the second checks that every file of a corpus lists through the native stream exactly what LibRaw's buffer stream lists for it in memory; the third is the [per-camera inventory](../../docs/research/embedded-previews.md), whose command is given there:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw \
  cargo test --release -p luxforge-raw --locked --test embedded_previews embedded_owner -- --ignored --nocapture
LUXFORGE_PREVIEW_DIRS=/path/to/selection:/path/to/popular:/path/to/owner/raw \
  cargo test --release -p luxforge-raw --locked --lib embedded_corpus -- --ignored --nocapture
```
