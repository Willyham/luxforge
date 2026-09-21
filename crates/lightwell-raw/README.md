# Lightwell RAW adapter

`lightwell-raw` is the only Lightwell crate with an explicit unsafe FFI boundary. It builds pinned native source locally; the rest of the workspace keeps its `forbid(unsafe_code)` rule. The safe API takes source bytes owned by the caller, unpacks one qualified RAW image on a worker, retains one immutable sensor mosaic, and develops green-normalized white-balance edits into one owned planar float allocation. It never rewrites an original or creates an intermediate file. [THIRD_PARTY.md](THIRD_PARTY.md) records provenance, notices, build flags, and the exact native source selection.

```rust
let source = RawSource::decode(verified_bytes, &cancel)?;
let metadata = source.metadata();
let camera_linear = source.develop(metadata.as_shot_gains, &cancel)?;
// camera_linear.data is [red plane, green plane, blue plane] in sensor order.
```

The caller must read/hash a stable original through the catalog's source-verification path before passing its `Arc<[u8]>`. This crate is a CPU worker operation and does not own catalog identity, history, scheduling, display conversion, or GPU upload. `decode` borrows the encoded bytes through LibRaw `open_buffer`, copies its u16 mosaic once into a Rust `Vec`, closes LibRaw, and drops the encoded bytes. The returned `RawSource` uses `Arc<Vec<u16>>`; clones share the sensor allocation. `develop` subtracts LibRaw scalar/channel/repeating black, normalizes each CFA sample by `sensor_white - black` to librtprocess's 65535 sensor scale, applies per-channel WB before demosaic, runs RCD for Bayer or one-pass Markesteijn for X-Trans, and divides the planar result by 65535. It does not clip at display white; RCD itself clamps negative reconstructed Bayer channels to zero, while Markesteijn can produce negative values. A WB change reruns from the immutable mosaic. Exposure, camera→working matrix and later edits can use the developed planes without rereading the source.

`RawMetadata` records the validated full sensor shape, active area, camera default crop, raw LibRaw inset, CFA phase and 2×2/6×6 pattern, black pattern, sensor white, as-shot green-normalized gains, EXIF orientation 1–8, and both LibRaw color arrays. `rgb_cam` is LibRaw's WB-balanced camera RGB→linear sRGB matrix; the caller applies it once after `develop` and must not apply `pre_mul` again. `cam_xyz` is preserved as source calibration provenance. Current Fuji framing uses RAF camera crop tags; LibRaw's inset trims three additional top rows and is exposed separately. Exact mode validation requires the Nikon Z6 lossless maker-note marker 3 at 12/14-bit or Fujifilm X100VI RAF compression marker 0/2 at 14-bit, with the expected full sensor dimensions, decoder path, CFA, and single frame. Extension or camera-name acceptance alone is insufficient.

DJI Mavic Air 2S DNG remains outside supported modes. The owner sample's OpcodeList3 contains required GainMap (9) and WarpRectilinear (1) operations. `required_dng_opcodes` inspects bounded TIFF IFD/opcode structures, and `decode` returns `UnsupportedRequiredOpcodes([1,9])` before unpack when mandatory operations are present. Optional or missing opcodes do not silently qualify a DNG; the DJI development/correction path still requires quality and geometry evidence.

## Bounds and liveness

| Allocation or resource | Bound / owner |
| --- | --- |
| Encoded source | 128 MiB Rust input limit; borrowed for the synchronous native decode only |
| Sensor shape | 16,384 per side and 64 million pixels, checked before Rust allocation and after native unpack; one frame only |
| Native unpack | LibRaw `max_raw_memory_mb=512`, plus Rust checked dimensions/stride; native temporary decoder closes before source publication |
| Retained u16 sensor mosaic | One Rust allocation, `2 × pixels`, shared by `Arc<Vec<u16>>` |
| Developed float output | One Rust `Vec<f32>` of `3 × pixels`, capped at 512 MiB; `[R plane,G plane,B plane]` |
| Demosaic input | One temporary float mosaic, `4 × pixels`, plus algorithm tile workspace and row-pointer tables; released before `develop` returns |
| Job concurrency | Owned by the editor worker; reserve for in-flight mosaic/output/old presentation before starting replacement |

At full X100VI sensor size (7872×5196 = 40,902,912 pixels), retained u16 mosaic is about 78 MiB, the developed three-plane output about 468 MiB, and temporary float mosaic about 156 MiB, before native decoder scratch, worker overlap, color/geometry and GPU storage. The adapter's per-buffer checks do not by themselves enforce the editor's combined-process memory target. The caller must bound active/pending workers and account for old visible results during replacement.

Cancellation is checked before decode, through LibRaw's synchronous progress callback during identify/unpack, every 128 input rows while preparing float data, and after librtprocess returns. The pinned librtprocess routines call a progress callback but ignore its return value, so cancellation during the demosaic stage prevents publication only after that stage finishes. No C++ exception crosses the ABI. A Rust `AtomicBool` is accessed only by an `extern "C"` callback during a synchronous native call; it is never reinterpreted as a C++ atomic or retained beyond the call.

## Evidence and limits

`cargo test -p lightwell-raw --locked` checks malformed container bounds, required/optional opcode flags, cycles, cancellation before work, and geometry. Local authentic tests run with:

```sh
LIGHTWELL_RAW_OWNER_DIR=/path/to/owner/raw \
LIGHTWELL_RAW_PUBLIC_DIR=/path/to/cc0/raw \
  cargo test --release -p lightwell-raw --locked --test real_files -- --ignored --nocapture
```

The authentic test compares every full sensor u16 buffer to an independently recorded LibRaw probe SHA-256, verifies source hashes before/after, mode, crop, CFA, white metadata, owner Z6 EXIF orientation, finite developed floats, retained subzero/above-one latitude, invalid gains and cancellation. On the owner's M4 Pro, it passed six Nikon/Fuji samples and explicitly rejected the required DJI opcodes. Probe inputs and comparisons live in [probes/raw](../../probes/raw/README.md); owner files and generated outputs stay ignored. Manual dependency/native/asset review and clean Windows/Linux package verification are still outstanding. This crate alone does not qualify the visible neutral look, color matrix tolerance, lens geometry, UI/API parity, export, or end-to-end latency.
