# Lightwell RAW adapter

`lightwell-raw` is the only Lightwell crate with an explicit unsafe FFI boundary. It builds pinned native source locally; the rest of the workspace keeps its `forbid(unsafe_code)` rule. The safe API takes source bytes owned by the caller, unpacks one qualified RAW image on a worker, retains one immutable sensor mosaic, and develops green-normalized white-balance edits (positive gains up to 32×) into one owned planar float allocation. It never rewrites an original or creates an intermediate file. [THIRD_PARTY.md](THIRD_PARTY.md) records provenance, notices, build flags, and the exact native source selection.

```rust
let source = RawSource::decode(verified_bytes, &cancel)?;
let metadata = source.metadata();
let camera_linear = source.develop(metadata.as_shot_gains, &cancel)?;
// camera_linear.data is [red plane, green plane, blue plane] in sensor order.
```

The caller must read/hash a stable original through the catalog's source-verification path before passing its `Arc<[u8]>`. This crate is a CPU worker operation and does not own catalog identity, history, scheduling, display conversion, or GPU upload. `decode` borrows the encoded bytes through LibRaw `open_buffer`, copies its u16 mosaic once into a Rust `Vec`, closes LibRaw, and drops the encoded bytes. The returned `RawSource` uses `Arc<Vec<u16>>`; clones share the sensor allocation. `develop` subtracts LibRaw scalar/channel/repeating black, normalizes each CFA sample by `sensor_white - black` to librtprocess's 65535 sensor scale, applies per-channel WB before demosaic, runs RCD for Bayer or one-pass Markesteijn for X-Trans, and divides the planar result by 65535. It does not clip at display white; RCD itself clamps negative reconstructed Bayer channels to zero, while Markesteijn can produce negative values. A WB change reruns from the immutable mosaic. Exposure, camera→working matrix and later edits can use the developed planes without rereading the source.

`RawMetadata` records the validated full sensor shape, active area, camera default crop, raw LibRaw inset, CFA phase and 2×2/6×6 pattern, black pattern, sensor white, as-shot green-normalized gains, EXIF orientation 1–8, and color matrices. `rgb_cam` is LibRaw's WB-balanced camera RGB→linear sRGB matrix; the caller applies it once after `develop` and must not apply `pre_mul` again. For Nikon and Fujifilm, `cam_xyz` is LibRaw's source calibration. For the qualified DJI DNG, LibRaw leaves that field zero, so the adapter validates and exposes the DNG's fixed D65 `ColorMatrix2` as the XYZ→camera matrix, with both source matrix hashes and illuminants in `dng_corrections`. This supports the existing fixed-matrix WB controls; it does not interpolate between illuminants. Current Fuji framing uses RAF camera crop tags; LibRaw's inset trims three additional top rows and is exposed separately. Exact mode validation requires the Nikon Z6 lossless maker-note marker 3 at 12/14-bit, Fujifilm X100VI RAF compression marker 0/2 at 14-bit, or the FC3411 16-bit uncompressed DNG sensor SubIFD and opcode layout, with the expected full sensor dimensions, decoder path, CFA, and single frame. Extension or camera-name acceptance alone is insufficient.

The qualified DJI Air 2S FC3411 DNG has required OpcodeList3 GainMap (9) followed by per-channel WarpRectilinear (1). `decode` associates them with the unique raw sensor SubIFD, validates their versions, area, finite values and warp geometry, and rejects unknown mandatory operations. `develop` applies the gain in active-area coordinates and then resamples each camera plane through its own chromatic warp before the caller's color matrix and default crop. It recognizes the exact identity green warp and retains those gained pixels without interpolation. It reuses one active-plane scratch buffer and keeps the full-sensor output layout. `corrected_sensor_sample_location` and `gain_at_sensor` provide bounded per-channel queries for the neutral picker. `dng_corrections` records the exact operation order, payload hashes, optional operations skipped and interpretation identity. This float path preserves negative values and highlight headroom. The DNG specification calls for clipping after OpcodeList2/3, so these float values are not a strict clipped DNG rendering when an intermediate value leaves [0,1]. This preserves the existing Lightwell RAW contract: scene headroom remains available to later exposure edits, with clipping only at terminal display.

## Bounds and liveness

| Allocation or resource | Bound / owner |
| --- | --- |
| Encoded source | 128 MiB Rust input limit; borrowed for the synchronous native decode only |
| Sensor shape | 16,384 per side and 64 million pixels, checked before Rust allocation and after native unpack; one frame only |
| Native unpack | LibRaw `max_raw_memory_mb=512`, plus Rust checked dimensions/stride; native temporary decoder closes before source publication |
| Retained u16 sensor mosaic | One Rust allocation, `2 × pixels`, shared by `Arc<Vec<u16>>` |
| Developed float output | One Rust `Vec<f32>` of `3 × pixels`, capped at 512 MiB; `[R plane,G plane,B plane]` |
| Demosaic input | One temporary float mosaic, `4 × pixels`, plus algorithm tile workspace and row-pointer tables; released before `develop` returns |
| DJI stage-three warp | One temporary active-area float plane, `4 × active pixels`, reused for three channels after native demosaic scratch is released |
| Job concurrency | Owned by the editor worker; reserve for in-flight mosaic/output/old presentation before starting replacement |

At full X100VI sensor size (7872×5196 = 40,902,912 pixels), retained u16 mosaic is about 78 MiB, the developed three-plane output about 468 MiB, and temporary float mosaic about 156 MiB, before native decoder scratch, worker overlap, color/geometry and GPU storage. The adapter's per-buffer checks do not by themselves enforce the editor's combined-process memory target. The caller must bound active/pending workers and account for old visible results during replacement.

Cancellation is checked before decode, through LibRaw's synchronous progress callback during identify/unpack, every 128 input rows while preparing float data, after librtprocess returns, and every 64 rows during each DJI gain/warp pass. The pinned librtprocess routines call a progress callback but ignore its return value, so cancellation during the demosaic stage prevents publication only after that stage finishes. No C++ exception crosses the ABI. A Rust `AtomicBool` is accessed only by an `extern "C"` callback during a synchronous native call; it is never reinterpreted as a C++ atomic or retained beyond the call.

## Evidence and limits

`cargo test -p lightwell-raw --locked` checks malformed container bounds, required/optional opcode flags, cycles, cancellation before work, and geometry. Local authentic tests run with:

```sh
LIGHTWELL_RAW_OWNER_DIR=/path/to/owner/raw \
LIGHTWELL_RAW_PUBLIC_DIR=/path/to/cc0/raw \
  cargo test --release -p lightwell-raw --locked --test real_files -- --ignored --nocapture
```

The authentic tests compare full sensor u16 buffers to independent LibRaw probe hashes, verify source hashes before/after, mode, crop, CFA, white metadata, owner Z6 EXIF orientation, finite developed floats, retained subzero/above-one latitude, invalid gains and cancellation. The DJI test also checks the opcode/calibration payload hashes, fixed matrix against LibRaw's rendered matrix, malformed mandatory operations, and 18 corrected camera-plane samples computed independently from sparse pre-correction pixels in [the DNG reference](../../probes/raw/dng_reference.json). The fixture itself and generated sparse dump stay outside the repository. Manual dependency/native/asset review and clean Windows/Linux package verification are still outstanding. This crate alone does not qualify visible color, export or end-to-end latency.
