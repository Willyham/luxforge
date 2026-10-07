# RAW camera profiles

Luxforge's camera policy lives in `crates/luxforge-raw/data/cameras.json`.
The catalog selects supported recording modes and implemented processing
capabilities. The modern-camera expansion and its approved resource bounds
are tracked in [corpus camera support](corpus-camera-support.md) and
[modern camera support](modern-camera-support.md).

## Format and ownership

The version-1 JSON catalog contains exact LibRaw make/model identities, sensor
size, CFA dimensions, and recording modes. Each mode records its identifier,
bit depth, raw frame count, decoder name, validation strategy, optional DNG
version, container compression probe and expected value, and, when the decoder
stores a different observed frame, that frame's size. A mode may override the
camera's crop and DNG settings with `processing`, allowing a native RAW and
a converted DNG to share an identity. Each camera selects
a crop source (`libraw_inset`, `raf_tags`, `active_area`, or `dng_tags`) and
optional DNG processing settings. DNG settings declare the
illuminant pair, fixed matrix selection, calibration/interpretation identities,
and required ordered opcode descriptors. Presence of those settings enables the
implemented DNG calibration and correction path. Typed choices prevent contradictory
boolean combinations; unsupported choices fail rather than silently disabling work.

Capture-dependent data remains authoritative in the original: black/white levels,
WB, CFA phase, crop coordinates, matrices, gain maps and warp coefficients are
read through LibRaw or the bounded container readers. They must not be replaced
with example-file values. Format signatures, TIFF tags, opcode layouts, numerical
algorithm constants and hard resource bounds remain implementation constants.
Pinned third-party LibRaw camera tables remain upstream-owned; this catalog owns
all Luxforge camera-specific policy, not a fork of the decompressor's internals.

Strict Rust types and semantic validation define the format: unknown fields,
unknown versions/strategies, duplicate identities/mode identifiers, ambiguous mode
selectors, invalid dimensions, and unsupported capability combinations fail the
build. The build then emits the validated catalog as static Rust data, so the
library parses no JSON at all, and a test proves that data equals the parsed
file. No runtime file lookup, environment override, download or JSON parsing is
introduced. Build-generated native allowlist entries come from the same
validated catalog, and a public recording mode is a reference to one of its
modes, serialized as the mode's identifier; an identifier the catalog does not
declare fails to deserialize. Unknown cameras, and unknown recording modes of
catalogued cameras, fail before unpack: the adapter classifies the mode from
LibRaw's identify-time metadata and the container before it unpacks.
Nikon High Efficiency (maker-note NEF compression 13 or 14, or JPEG XS markers at
the raw strip) is refused before the catalog is consulted, whatever the model, so
no mode can admit it ([popular camera support](popular-camera-support.md)).

## Field reference

All objects reject unknown keys. Arrays have the exact lengths shown. Nullable
fields may be omitted; omitted calibration uses the backend matrix.

| Object / field | Meaning |
| --- | --- |
| `version`, `cameras` | Current format marker `1`; 1–512 camera profiles; file capped at 1 MiB |
| Camera `make`, `model` | Exact case-sensitive identities returned by pinned LibRaw; unique pair, printable ASCII, fewer than 64 bytes each |
| `sensor_size` | `[width, height]` in full sensor pixels; subject to the decoder and float-buffer limits |
| `cfa_size`, `channels` | `[2, 2]` for Bayer/RCD or `[6, 6]` for X-Trans/one-pass Markesteijn, with one retained channel (default); `[0, 0]` with one channel for monochrome or three for linear RGB. Phase remains file metadata |
| `crop` | `libraw_inset`, `raf_tags`, `active_area`, or `dng_tags`; selects authoritative crop metadata |
| Camera `calibration` | Optional non-DNG `{xyz_to_camera: [[number; 3]; 3], source: HTTPS URL, license: string}`. A source-attributed fixed matrix, converted through pinned LibRaw; finite bounded nonsingular values required. Omit for backend calibration; forbidden alongside DNG source calibration |
| `modes` | 1–32 distinct recording-mode selectors: no two modes of a camera share bit depth, frame count, stored frame, decoder, DNG version and compression probe |
| Mode `id` | Globally unique alphanumeric identifier starting with an uppercase letter, at most 80 characters, excluding `Self`; also the generated Rust enum variant and serialized metadata label |
| `bits`, `raw_count`, `decoder` | Integer precision/storage depth, LibRaw raw-frame count (1 or 2), and exact decoder name |
| `validation` | `container_compression` for an explicit container marker, or `decoder_metadata` for validated decoder metadata |
| `compression` | `null`, or `{ "probe": "nef_maker_note" or "raf_header", "value": integer }`; absent/malformed source markers never match |
| `dng_version` | Packed DNG version integer or `null`; `17039360` is `0x01040000` |
| Mode `frame_size` | Optional exact observed `[width, height]` for a padded, cropped or converted recording mode. Different from `sensor_size` and within the same resource limits. The file still supplies the authoritative active area and default crop. Omit when the stored frame is the sensor |
| `unpacker` | Optional: `libraw` (the default when omitted) fills the mosaic with LibRaw's own decoder; `rawspeed` fills it with RawSpeed in place of that decoder, and is accepted only when `decoder` is in the code-owned replaceable table (`crates/luxforge-raw/src/unpacker.rs`). `jxl` uses LibRaw identification and the bounded Rust JPEG XL path for a single-segment 16-bit linear RGB DNG. Any other value fails the build. Route a mode only with authentic evidence that its mosaic equals the LibRaw-only hash ([RawSpeed unpacking](rawspeed-unpack.md)) |
| Camera `dng` | `null` for backend calibration, or the complete DNG settings object below; enabled together with `dng_tags` |
| Mode `processing` | Optional `{crop, dng}` selecting the effective processing settings for this observed mode |
| DNG `container` | `uncompressed_u16_single_strip`, `integer_cfa_single_segment` or `integer_linear_single_segment` for the strict single-segment paths; `integer_cfa_segments` or `integer_linear_segments` for bounded strips/tiles and source-authoritative ActiveArea. All require integer storage and integral crop; at most 65,536 nonoverlapping, in-file segments |
| `calibration` | `root_fixed_matrix` preserves the strict existing path. `source_reference_matrix` reads root source ColorMatrix, CameraCalibration, AnalogBalance and AsShotNeutral or AsShotWhiteXY, validating both matrices and any ForwardMatrix without applying a DCP profile or illuminant interpolation. `monochrome` has no colour matrix or white balance |
| `illuminants`, `selected_matrix` | Expected two DNG illuminant IDs; select matrix `1` or `2` for fixed XYZ-to-camera calibration |
| `calibration_identity` | Persisted description of the selected calibration; update when its interpretation changes |
| `corrections` | `stage3_gain_map_then_warp` for the GainMap→Warp path, or `stage_ordered` for bounded ordered opcode processing |
| `required_opcodes` | 0–8 ordered descriptors `{id, list, version, flags}`. Supported IDs, from the one allowlist in `crates/luxforge-raw/src/opcodes.rs`, are GainMap 9, WarpRectilinear 1, FixVignetteRadial 3, FixBadPixelsConstant 4, and FixBadPixelsList 5; list 51022 for stage-three operations, list 51009 for CFA GainMap and list 51008 for sensor repairs or pre-black FixVignetteRadial, version 16973824 (`0x01030000`), flags 0 |
| `interpretation` | Persisted correction interpretation identity; update when processing semantics change |
| `decoder_active_bottom_trim` | Optional integer 0–63. For a decoder whose reported active bottom is shorter than the authoritative DNG `ActiveArea`, requires the source bottom to equal decoder bottom plus this exact trim; all other edges must match. Omit when no decoder trim is needed. |

Monochrome calibration is valid only for a one-channel non-CFA layout. The
sampled monochrome path accepts no required DNG correction opcodes. Linear
RGB accepts supported stage-three corrections; pre-demosaic sensor corrections
remain CFA-only. Incompatible combinations fail catalog validation.

The opcode recipe is validated against the implemented algorithm's supported
order, stage, version and flags. Stage-ordered processing preserves source
operation order and uses bounded sparse immutable mosaic patches for bad-pixel
operations; the retained source integer samples are never rewritten. Vignette evaluation
follows DNG normalized outer-pixel coordinates, while the bounded implementation
records its interpretation rather than claiming blanket SDK bit identity.
Changing data cannot enable an unimplemented
opcode or relax global resource bounds. The container strategy also rejects
nonunity scaling, fractional crops, unsupported channel layouts and ambiguous
calibration; these are algorithm constraints shared by every profile selecting it.

To add a camera, create a unique make/model entry with mode selectors from
actual decoder/container evidence, choose existing crop and processing
capabilities, and run the RAW unit and authentic-file tests plus editor
verification. `cargo build -p luxforge-raw --locked` validates the catalog and
regenerates both language tables. Update the support/coverage documentation only
after the new recording modes are demonstrated. Changing a profile does not
rewrite catalogs: incompatible source interpretation still fails explicitly.

Reproduce corpus selection and decoder evidence with
`cargo xtask raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW_DIR`
and the generated `fixtures/modern-camera-selection.json`; inspect DNG geometry
and ordered opcodes with `cargo xtask inspect-dng --source PATH`. The checked-in
selection file records source URLs, hashes and licensing metadata; downloaded
RAW payloads and generated outputs remain outside the repository.

## Acceptance

- No Luxforge production branch selects processing by camera name or mode ID.
- The catalog contains 259 camera/phone profiles and 316 recording modes.
  The retained 341-source corpus freezes 315 modes on 258 exact identities;
  the owner Air 2S fixture supplies the remaining profile/mode. Every retained
  source passes strict M4 decode/development regression. Controlled colour and
  unsampled recording settings remain separate.
- Camera addition using existing strategies requires only data; implementing a
  new format/algorithm still requires code, tests and authentic qualification.
- Invalid catalogs fail explicitly, with no partially accepted entries.
- Tests cover profile mutation, malformed/ambiguous catalogs, unknown cameras,
  mode rejection and required-correction selection. Authentic source/mosaic and
  DNG numerical references plus background editor evidence verify preservation.
- New coverage runs quick and rendered verification; completed feature work
  carries scoped timing and full verification before a broader support claim.
  GPU-rendered stacks remain within the declared tolerance of the whole-frame
  CPU reference; decode and development references keep their exact contracts.

## Performance checklist

Source reads, hashing and unpack still use the verified source worker/cache.
Profiles add a bounded immutable catalog. Sensor repairs add at most 65,536
sorted sparse patches shared with the source, with no additional full-frame
mosaic copy. New source-segment sensor repairs exclude masked padding, while
the existing strict paths retain their frozen full-frame repair interpretation.
Stage-one vignette gain applies before black subtraction; stage-two CFA gain
maps multiply normalized sensor values before demosaic. Neutral sampling validates the bounded patch list and uses binary
search per sampled site; it never develops or renders a frame. Owner work,
desktop refreshes and timers gain no frame processing. Native allowlist lookup and mode classification run before unpack. Existing
exact mosaic, correction, geometry and sharing tests remain applicable. Timing
verification covers photo-sized workloads; this refactor claims no speedup.
