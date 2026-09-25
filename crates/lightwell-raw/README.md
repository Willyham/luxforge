# Lightwell RAW adapter

`lightwell-raw` is the only Lightwell crate with an explicit unsafe FFI boundary. It builds pinned native source locally; the rest of the workspace keeps its `forbid(unsafe_code)` rule. The safe API takes source bytes owned by the caller, unpacks one qualified RAW image on a worker, retains one immutable sensor mosaic, and develops green-normalized white-balance edits (positive gains up to 32×) into one owned planar float allocation. It never rewrites an original or creates an intermediate file. [THIRD_PARTY.md](THIRD_PARTY.md) records provenance, notices, build flags, and the exact native source selection.

```rust
let source = RawSource::decode(verified_bytes, &cancel)?;
let metadata = source.metadata();
let camera_linear = source.develop(metadata.as_shot_gains, &cancel)?;
// camera_linear.data is [red plane, green plane, blue plane] in sensor order.
```

The caller must read/hash a stable original through the catalog's source-verification path before passing its `Arc<[u8]>`. This crate is a CPU worker operation and does not own catalog identity, history, scheduling, display conversion, or GPU upload. `decode` borrows the encoded bytes through LibRaw `open_buffer`, copies its u16 mosaic once into a Rust `Vec`, closes LibRaw, and drops the encoded bytes. The returned `RawSource` uses `Arc<Vec<u16>>`; clones share the sensor allocation. `develop` subtracts LibRaw scalar/channel/repeating black, normalizes each CFA sample by `sensor_white - black` to librtprocess's 65535 sensor scale, applies per-channel WB before demosaic, runs RCD for Bayer or one-pass Markesteijn for X-Trans, and divides the planar result by 65535. It does not clip at display white; RCD itself clamps negative reconstructed Bayer channels to zero, while Markesteijn can produce negative values. A WB change reruns from the immutable mosaic. Exposure, camera→working matrix and later edits can use the developed planes without rereading the source.

`RawMetadata` records the validated full sensor shape, active area, camera default crop, raw LibRaw inset, CFA phase and 2×2/6×6 pattern, black pattern, sensor white, as-shot green-normalized gains, EXIF orientation 1–8, and color matrices. `rgb_cam` is LibRaw's WB-balanced camera RGB→linear sRGB matrix; the caller applies it once after `develop` and must not apply `pre_mul` again. For non-DNG modes, `cam_xyz` is LibRaw's calibration unless the profile supplies a source-attributed fixed matrix. Configured matrices use the pinned LibRaw coefficient conversion; missing or singular XYZ-to-camera calibration fails explicitly. For DNG modes the adapter validates and exposes the profile-selected source `ColorMatrix` as the XYZ→camera matrix, with both source matrix hashes and illuminants in `dng_corrections`. This supports the existing fixed-matrix WB controls; it does not interpolate between illuminants. Current Fuji framing uses RAF camera crop tags; LibRaw's inset trims three additional top rows and is exposed separately. Exact mode validation matches configured sensor dimensions, decoder, bits, CFA dimensions and raw frame count. Container validation additionally checks explicit compression markers where selected, including Z6 lossless maker-note marker 3 and X100VI RAF header marker 0/2. DNG modes validate storage, calibration, crop and the configured opcode layout. The primary image is selected from a declared one- or two-frame container; secondary Dual Pixel data is not blended. Extension or camera-name acceptance alone is insufficient.

The qualified DJI Air 2S FC3411 DNG has required OpcodeList3 GainMap (9) followed by per-channel WarpRectilinear (1). `decode` associates them with the unique raw sensor SubIFD, validates their versions, area, finite values and warp geometry, and rejects unknown mandatory operations. `develop` applies the gain in active-area coordinates and then resamples each camera plane through its own chromatic warp before the caller's color matrix and default crop. It recognizes the exact identity green warp and retains those gained pixels without interpolation. It reuses one active-plane scratch buffer and keeps the full-sensor output layout. `corrected_sensor_sample_location` and `gain_at_corrected_sensor` provide bounded per-channel queries for the neutral picker. `dng_corrections` records the exact operation order, payload hashes, optional operations skipped and interpretation identity. This float path preserves negative values and highlight headroom. The DNG specification calls for clipping after OpcodeList2/3, so these float values are not a strict clipped DNG rendering when an intermediate value leaves [0,1]. This preserves the existing Lightwell RAW contract: scene headroom remains available to later exposure edits, with clipping only at terminal display.

The public metadata includes validated `active_area`, `default_crop`, `black_cfa`,
CFA and calibration fields. The selected mode declares the validated raw frame count. The current catalog contains 100
camera profiles and 103 recording modes; resource and visible-quality
qualification remains ongoing.

The ordered DNG path also recognizes FixVignetteRadial (3),
FixBadPixelsConstant (4), and FixBadPixelsList (5). Bad-pixel operations produce
bounded sparse replacements from an immutable source mosaic; unresolved points
remain unchanged and are reported. Vignette evaluation follows DNG normalized
outer-pixel coordinates; this implementation records its interpretation rather
than claiming blanket SDK bit identity.

## Camera catalog

[data/cameras.json](data/cameras.json) is the single source for Lightwell camera
identities, sensor dimensions, CFA dimensions, recording-mode selectors, crop
source and DNG calibration/correction settings. The [format contract](../../docs/design/raw-camera-profiles.md)
describes the fields and how to add a profile. The strict parser in
[src/profiles.rs](src/profiles.rs) validates the catalog at build time; the build
produces the native pre-unpack allowlist and public mode identifiers from it.
The same JSON is embedded and parsed once on first use, without runtime file I/O.
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
| Native unpack | LibRaw `max_raw_memory_mb=512`, plus Rust checked dimensions/stride; native temporary decoder closes before source publication |
| Retained u16 sensor mosaic | One Rust allocation, `2 × pixels`, shared by `Arc<Vec<u16>>` |
| Developed float output | One Rust `Vec<f32>` of `3 × pixels`, capped at 1.5 GiB per planar RGB buffer; `[R plane,G plane,B plane]` |
| Demosaic input | One temporary float mosaic, `4 × pixels`, plus algorithm tile workspace and row-pointer tables; released before `develop` returns |
| Demosaic tile scratch | One native heap allocation per admitted callback, 988,208 bytes for X-Trans Markesteijn and 978,536 for Bayer RCD, at most eight process-wide (7,905,664 bytes total), plus callback stack arrays outside that heap count. The shared Rayon pool runs bounded ordinary tile jobs; the final job, which carries the frame's last tile rows, runs on the source caller. No private thread pool or full-frame scratch copy. |
| DNG stage-three warp | One temporary active-area float plane, `4 × active pixels`, reused for three channels after native demosaic scratch is released |
| Job concurrency | Owned by the editor worker; reserve for in-flight mosaic/output/old presentation before starting replacement |

At full X100VI sensor size (7872×5196 = 40,902,912 pixels), retained u16 mosaic is about 78 MiB, the developed three-plane output about 468 MiB, and temporary float mosaic about 156 MiB, before native decoder scratch, worker overlap, color/geometry and GPU storage. The adapter's per-buffer checks do not by themselves enforce the editor's combined-process memory target. The caller must bound active/pending workers and account for old visible results during replacement.

Cancellation is checked before decode, through LibRaw's synchronous progress callback during identify/unpack, every 128 input rows while preparing float data (every row when Bayer rows run on the pool), before every RCD or Markesteijn tile, after librtprocess returns, and every 64 rows during each DNG gain/warp pass. Both demosaics run bounded tile jobs with process-wide scratch admission; each job is a contiguous run of the serial tile raster that begins at a full tile, so partial edge tiles keep the scratch history they read. X-Trans dimensions below 120 px and Bayer dimensions below 10 px on either side fail explicitly. See [the execution and lifetime contract](../../docs/design/native-demosaic-parallelism.md). No C++ exception crosses the ABI. A Rust `AtomicBool` is accessed only by an `extern "C"` callback during a synchronous native call; it is never reinterpreted as a C++ atomic or retained beyond the call.

## Evidence and limits

`cargo test -p lightwell-raw --locked` checks malformed container bounds, required/optional opcode flags, cycles, cancellation before work, and geometry. Local authentic tests run with:

```sh
LIGHTWELL_RAW_OWNER_DIR=/path/to/owner/raw \
LIGHTWELL_RAW_PUBLIC_DIR=/path/to/cc0/raw \
  cargo test --release -p lightwell-raw --locked --test real_files -- --ignored --nocapture
```

The complete Markesteijn serial/pool oracle, including changed white balance, edge geometry, concurrent callers and native fault recovery, runs separately:

```sh
LIGHTWELL_RAW_OWNER_DIR=/path/to/owner/raw \
  cargo test --release -p lightwell-raw --locked --lib markesteijn_parallel_complete_float_oracle -- --ignored --nocapture
```

The Bayer oracle checks the serial RGB planes against an immutable pre-change M4 digest and compares complete normalized mosaics and RGB planes from the serial raster with pooled RCD at one, two, four and the default number of workers, and with changed white balance, one source per process (`nikon_z6.NEF` or `mavic_air_2s.DNG`):

```sh
LIGHTWELL_RAW_OWNER_DIR=/path/to/owner/raw LIGHTWELL_RAW_PROFILE_SOURCE=nikon_z6.NEF \
  cargo test --release -p lightwell-raw --locked --lib bayer_owner_mosaic_and_rgb_oracle -- --ignored --nocapture
```

The owner and public corpus checks are separate tests. If only owner originals are available, run the same command with `--skip authentic_public_modes` and report the public modes as untested.

The authentic tests compare full sensor u16 buffers to independent LibRaw probe hashes, verify source hashes before/after, mode, crop, CFA, white metadata, owner Z6 EXIF orientation, finite developed floats, retained subzero/above-one latitude, invalid gains and cancellation. The DJI test also checks the opcode/calibration payload hashes, fixed matrix against LibRaw's rendered matrix, malformed mandatory operations, and 18 corrected camera-plane samples computed independently from sparse pre-correction pixels in [the DNG reference](../../probes/raw/dng_reference.json). The fixture itself and generated sparse dump stay outside the repository. Manual dependency/native/asset review and clean Windows/Linux package verification are still outstanding. This crate alone does not qualify visible color, export or end-to-end latency.
