# RAW camera profiles

Lightwell's camera policy lives in `crates/lightwell-raw/data/cameras.json`.
The catalog selects supported recording modes and implemented processing
capabilities. The modern-camera expansion and its approved resource bounds
are tracked in [modern camera support](modern-camera-support.md).

## Format and ownership

The version-1 JSON catalog contains exact LibRaw make/model identities, sensor
size, CFA dimensions, and recording modes. Each mode records its identifier,
bit depth, raw frame count, decoder name, validation strategy, optional DNG
version, and container compression probe and expected value. Each camera selects
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
all Lightwell camera-specific policy, not a fork of the decompressor's internals.

Strict Rust types and semantic validation define the format: unknown fields,
unknown versions/strategies, duplicate identities/mode identifiers, ambiguous mode
selectors, invalid dimensions, and unsupported capability combinations fail the
build. The build then emits the validated catalog as static Rust data, so the
library parses no JSON at all, and a test proves that data equals the parsed
file. No runtime file lookup, environment override, download or JSON parsing is
introduced. Build-generated native allowlist entries and Rust mode identifiers
come from the same validated catalog. Unknown cameras still fail before unpack.

## Field reference

All objects reject unknown keys. Arrays have the exact lengths shown. Nullable
fields may be omitted; omitted calibration uses the backend matrix.

| Object / field | Meaning |
| --- | --- |
| `version`, `cameras` | Current format marker `1`; 1–256 camera profiles; file capped at 1 MiB |
| Camera `make`, `model` | Exact case-sensitive identities returned by pinned LibRaw; unique pair, printable ASCII, fewer than 64 bytes each |
| `sensor_size` | `[width, height]` in full sensor pixels; subject to the decoder and float-buffer limits |
| `cfa_size` | `[2, 2]` for Bayer/RCD or `[6, 6]` for X-Trans/one-pass Markesteijn; phase remains file metadata |
| `crop` | `libraw_inset`, `raf_tags`, `active_area`, or `dng_tags`; selects authoritative crop metadata |
| Camera `calibration` | Optional non-DNG `{xyz_to_camera: [[number; 3]; 3], source: HTTPS URL, license: string}`. A source-attributed fixed matrix, converted through pinned LibRaw; finite bounded nonsingular values required. Omit for backend calibration; forbidden alongside DNG source calibration |
| `modes` | 1–32 distinct recording-mode selectors |
| Mode `id` | Globally unique alphanumeric identifier starting with an uppercase letter, at most 80 characters, excluding `Self`; also the generated Rust enum variant and serialized metadata label |
| `bits`, `raw_count`, `decoder` | Integer precision/storage depth, LibRaw raw-frame count (1 or 2), and exact decoder name |
| `validation` | `container_compression` for an explicit container marker, or `decoder_metadata` for validated decoder metadata |
| `compression` | `null`, or `{ "probe": "nef_maker_note" or "raf_header", "value": integer }`; absent/malformed source markers never match |
| `dng_version` | Packed DNG version integer or `null`; `17039360` is `0x01040000` |
| Camera `dng` | `null` for backend calibration, or the complete DNG settings object below; enabled together with `dng_tags` |
| DNG `container` | `uncompressed_u16_single_strip` or `integer_cfa_single_segment`: one-channel integer CFA storage with matching sensor geometry and integral crop |
| `calibration` | `root_fixed_matrix`: both source matrices required, identity AnalogBalance, no alternate calibration/forward profiles |
| `illuminants`, `selected_matrix` | Expected two DNG illuminant IDs; select matrix `1` or `2` for fixed XYZ-to-camera calibration |
| `calibration_identity` | Persisted description of the selected calibration; update when its interpretation changes |
| `corrections` | `stage3_gain_map_then_warp` for the GainMap→Warp path, or `stage_ordered` for bounded ordered opcode processing |
| `required_opcodes` | 0–8 ordered descriptors `{id, list, version, flags}`. Supported IDs, from the one allowlist in `crates/lightwell-raw/src/opcodes.rs`, are GainMap 9, WarpRectilinear 1, FixVignetteRadial 3, FixBadPixelsConstant 4, and FixBadPixelsList 5; list 51022 for stage-three operations and at most one list-51008 sensor repair, version 16973824 (`0x01030000`), flags 0 |
| `interpretation` | Persisted correction interpretation identity; update when processing semantics change |
| `decoder_active_bottom_trim` | Optional integer 0–63. For a decoder whose reported active bottom is shorter than the authoritative DNG `ActiveArea`, requires the source bottom to equal decoder bottom plus this exact trim; all other edges must match. Omit when no decoder trim is needed. |

The opcode recipe is validated against the implemented algorithm's supported
order, stage, version and flags. Stage-ordered processing preserves source
operation order and uses bounded sparse immutable mosaic patches for bad-pixel
operations; the retained source mosaic is never rewritten. Vignette evaluation
follows DNG normalized outer-pixel coordinates, while the bounded implementation
records its interpretation rather than claiming blanket SDK bit identity.
Changing data cannot enable an unimplemented
opcode or relax global resource bounds. The container strategy also rejects
nonunity scaling, fractional crops, unsupported channel layouts and ambiguous
calibration; these are algorithm constraints shared by every profile selecting it.

To add a camera, create a unique make/model entry with mode selectors from
actual decoder/container evidence, choose existing crop and processing
capabilities, and run the RAW unit and authentic-file tests plus editor
verification. `cargo build -p lightwell-raw --locked` validates the catalog and
regenerates both language tables. Update the support/coverage documentation only
after the new recording modes are demonstrated. Changing a profile does not
rewrite catalogs: incompatible source interpretation still fails explicitly.

Reproduce corpus selection and decoder evidence with
`python3 probes/raw/collect_camera_metadata.py --help` and the generated
`fixtures/modern-camera-selection.json`; inspect DNG geometry and ordered
opcodes with `python3 probes/raw/inspect_dng.py PATH`. The checked-in selection
file records source URLs, hashes and licensing metadata; downloaded RAW payloads
and generated probe outputs remain outside the repository.

## Acceptance

- No Lightwell production branch selects processing by camera name or mode ID.
- The current catalog contains 100 camera profiles and 103 recording modes.
  One authentic selected mode per model has adapter evidence; controlled color
  and broader recording-mode qualification remain separate.
- Camera addition using existing strategies requires only data; implementing a
  new format/algorithm still requires code, tests and authentic qualification.
- Invalid catalogs fail explicitly, with no partially accepted entries.
- Tests cover profile mutation, malformed/ambiguous catalogs, unknown cameras,
  mode rejection and required-correction selection. Authentic source/mosaic and
  DNG numerical references plus background editor evidence verify preservation.
- Run quick, rendered and timing verification. Delete the completed temporary
  task plan per repository convention.

## Performance checklist

Source reads, hashing and unpack still use the verified source worker/cache.
Profiles add a bounded immutable catalog. Sensor repairs add at most 65,536
sorted sparse patches shared with the source, with no additional full-frame
mosaic copy. Neutral sampling validates the bounded patch list and uses binary
search per sampled site; it never develops or renders a frame. Owner work,
desktop refreshes and timers gain no frame processing. Native allowlist lookup remains before unpack. Existing
exact mosaic, correction, geometry and sharing tests remain applicable. Timing
verification covers photo-sized workloads; this refactor claims no speedup.
