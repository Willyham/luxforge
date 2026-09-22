# RAW camera profiles

Lightwell's camera policy lives in `crates/lightwell-raw/data/cameras.json`.
The catalog selects supported recording modes and existing processing capabilities;
it does not widen the currently qualified camera/mode set.

## Format and ownership

The version-1 JSON catalog contains exact LibRaw make/model identities, sensor
size, CFA dimensions, and recording modes. Each mode records its identifier,
bit depth, decoder name, optional DNG version, and container compression probe
and expected value. Each camera selects a crop source (`libraw_inset`, `raf_tags`,
or `dng_tags`) and optional DNG processing settings. DNG settings declare the
illuminant pair, fixed matrix selection, calibration/interpretation identities,
and required ordered opcode descriptors. Presence of those settings enables the
implemented stage-three gain/warp capability. Typed choices prevent contradictory
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
build. The same parser loads the embedded catalog once, on first use. No
runtime file lookup, environment override, download or per-image JSON parsing is
introduced. Build-generated native allowlist entries and Rust mode identifiers
come from the same validated catalog. Unknown cameras still fail before unpack.

## Field reference

All objects reject unknown keys. Arrays have the exact lengths shown. Nullable
fields may be omitted; the bundled file writes them explicitly for readability.

| Object / field | Meaning |
| --- | --- |
| `version`, `cameras` | Current format marker `1`; 1–256 camera profiles; file capped at 1 MiB |
| Camera `make`, `model` | Exact case-sensitive identities returned by pinned LibRaw; unique pair, printable ASCII, fewer than 64 bytes each |
| `sensor_size` | `[width, height]` in full sensor pixels; subject to the decoder and float-buffer limits |
| `cfa_size` | `[2, 2]` for Bayer/RCD or `[6, 6]` for X-Trans/one-pass Markesteijn; phase remains file metadata |
| `crop` | `libraw_inset`, `raf_tags`, or `dng_tags`; selects authoritative crop metadata |
| `modes` | 1–32 distinct recording-mode selectors |
| Mode `id` | Globally unique alphanumeric identifier starting with an uppercase letter, at most 80 characters, excluding `Self`; also the generated Rust enum variant and serialized metadata label |
| `bits`, `decoder` | Integer precision/storage depth and exact LibRaw decoder name |
| `compression` | `null`, or `{ "probe": "nef_maker_note" or "raf_header", "value": integer }`; absent/malformed source markers never match |
| `dng_version` | Packed DNG version integer or `null`; `17039360` is `0x01040000` |
| Camera `dng` | `null` for backend calibration, or the complete DNG settings object below; enabled together with `dng_tags` |
| DNG `container` | `uncompressed_u16_single_strip`: one-channel 16-bit CFA strip, unity scale, integral crop, matching sensor geometry |
| `calibration` | `root_fixed_matrix`: both source matrices required, identity AnalogBalance, no alternate calibration/forward profiles |
| `illuminants`, `selected_matrix` | Expected two DNG illuminant IDs; select matrix `1` or `2` for fixed XYZ-to-camera calibration |
| `calibration_identity` | Persisted description of the selected calibration; update when its interpretation changes |
| `corrections` | `stage3_gain_map_then_warp`: implemented unclipped camera-RGB gain followed by per-channel warp |
| `required_opcodes` | Ordered descriptors `{id, list, version, flags}`; currently GainMap 9 then WarpRectilinear 1 in list 51022, version 16973824 (`0x01030000`), flags 0 |
| `interpretation` | Persisted correction interpretation identity; update when processing semantics change |

The opcode recipe is validated against the implemented algorithm's supported
order, stage, version and flags. Changing data cannot enable an unimplemented
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

## Acceptance

- No Lightwell production branch selects processing by camera name or mode ID.
- The five current modes retain their identifiers, metadata and numerical output.
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
Profiles add only a small bounded immutable catalog; no new full-frame
buffers or copies. Point queries, no-op validation, owner work, desktop refreshes,
and timers are unchanged. Native allowlist lookup remains before unpack. Existing
exact mosaic, correction, geometry and sharing tests remain applicable. Timing
verification covers photo-sized workloads; this refactor claims no speedup.
