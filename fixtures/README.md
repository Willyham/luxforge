# Synthetic fixtures

The sixteen checked-in JPEGs under `s0/` are synthetic golden inputs covered by the repository license. `s0/manifest.json` records SHA-256, encoded dimensions, mode, EXIF orientation, expected displayed dimensions and expected outcome per file.

```sh
cargo xtask fixtures
cargo xtask generate-fixtures [--output NEW_DIRECTORY]
```

`fixtures` verifies golden hashes, dimensions, orientation, greyscale and profile presence, supported or error outcomes and independent corner expectations, and checks that the Rust generator is repeatable. `generate-fixtures` writes 24 MP (6000×4000) and 60 MP (10000×6000) untagged sRGB workloads into a new ignored `fixtures/generated/` directory and refuses an existing one. The content checks are not independent ICC colorimetry.

Each landscape fixture has red, green, blue and gold quadrants (top-left, top-right, bottom-left, bottom-right), corner labels, an upward arrow and fine vertical detail, so orientation and reflection errors are visible.

| Input | Expected behavior |
| --- | --- |
| `orientation-1.jpg` to `orientation-8.jpg` | 480×320 encoded, untagged sRGB. Orientations 1 to 4 display 480×320; 5 to 8 display 320×480 |
| `portrait.jpg` | 320×480, EXIF 1, untagged sRGB |
| `greyscale.jpg` | 480×320 luminance; neutral output after RGB expansion |
| `srgb.jpg` | Landscape with a valid embedded synthetic sRGB ICC profile |
| `cmyk.jpg` | Valid CMYK JPEG; explicit unsupported-color error |
| `invalid-profile.jpg` | RGB JPEG with an invalid ICC payload; explicit unsupported-profile error |
| `invalid.jpg`, `truncated.jpg` | Invalid header and incomplete entropy stream; report failure and keep the last successful image |
| `oversized.jpg` | Header declares 65535×65535; rejected before raster allocation |

History tests build fresh catalogs and recipes through the current service and API using the synthetic JPEG fixtures. They verify persistence, undo/redo, restore, versions, exact pixels and source preservation against the current contracts.

Private originals for local diagnostics go in ignored `fixtures/jpg/`, `fixtures/raw/` or `private/` and are never committed. Crop and straightening geometry goldens (rotated-box mapping, output rounding, coverage and fitting, at off-center, near-boundary, portrait, landscape and every ±45° reference angle) live as code tests in `crates/luxforge-core/src/modules/crop/geometry.rs`; no new binary fixtures were added for M4. Later work still needs export profile and metadata fixtures, moved and changed originals for Locate, Adobe RGB and Display P3 inputs before any wide-gamut claim, and broader RAW quality-scene coverage. Owner-supplied Nikon Z6, Fujifilm X100VI and DJI Air 2S originals are available locally and excluded from Git.

[`mixer/mixer-cases.json`](mixer/README.md) is the frozen colour mixer's oracle: 414 input/parameter/expected-output cases (the eight sRGB range-centre colours, an achromatic ramp, near-black chroma, a hue wheel at three chroma levels, skin-like patches and out-of-gamut linear inputs, under nine parameter sets) computed by the independent `f64` reference in `crates/luxforge-core/tests/reference/mixer.rs`, reloaded and checked against a fresh computation by `crates/luxforge-core/tests/mixer_reference.rs`. See [docs/design/mixer-study.md](../docs/design/mixer-study.md) for the frozen equations, constants and tolerance this fixture proves; no production code reads it yet.
[`presence/`](presence/README.md) holds the frozen Presence study's oracle: deterministic image specs (step edges, encoded sinusoids, an 8-bit quantised ramp, seeded noise, textured patches and synthetic hazed scenes built from the forward model with a known transmission and atmospheric light) with the linear-sRGB output and the measured halo, band-response, banding, noise and haze-recovery figures computed by the independent `f64` reference in `crates/luxforge-core/tests/reference/presence.rs`. Neither file stores a pixel buffer; both are reloaded and recomputed by `crates/luxforge-core/tests/presence_reference.rs` on every run. See [docs/design/presence-study.md](../docs/design/presence-study.md) for the frozen equations, halos and tolerance these fixtures prove.

`basic/tone-cases.json` is the frozen global Tone algorithm's oracle: input/parameter/expected-output cases computed by the independent `f64` reference in `crates/luxforge-core/tests/reference/tone.rs`, reloaded and checked against a fresh computation by `crates/luxforge-core/tests/basic_tone_reference.rs`. See [docs/design/basic-tone.md](../docs/design/basic-tone.md) for the frozen equations this fixture proves.

[`vignette/`](vignette/README.md) holds the frozen post-crop vignette mask and amount equations' oracle: discrete `(pixel, parameters)` mask samples and full `apply` output cases, computed by the independent `f64` reference in `crates/luxforge-core/tests/reference/vignette.rs` and reloaded and checked against a fresh computation by `crates/luxforge-core/tests/vignette_reference.rs`. See [docs/design/vignette-study.md](../docs/design/vignette-study.md) for the frozen equations, worked examples and tolerance this fixture proves.

[`presets/`](presets/README.md) holds hand-written synthetic presets for the importer: Lightroom XMP develop presets, a profile, a sidecar, a legacy `.lrtemplate` and a Luxforge preset document. None is a copy of a third-party preset. The importer tests assert each one's exact settings, report and origin.

## RAW preparation fixtures

[Public provenance](raw-public.json) identifies four CC0 files from raw.pixls.us covering Z6 12/14-bit lossless and X100VI uncompressed/lossless capture. No photograph bytes are checked in. Paths resolve from the manifest directory; obtain the individual files from their recorded URLs and verify hashes before decoder experiments. The owner originals have a separate ignored local manifest with explicit permission for local testing only.

[Coverage](raw-coverage.json) distinguishes editor support from a decoder experiment. `untested` means not qualified in the editor, even when a probe can unpack the file. Real-file provenance, missing scenes, unknown metadata and unqualified modes remain visible.

```sh
cargo xtask raw-corpus --manifest fixtures/raw-public.json --output artifacts/new-raw-corpus
```

This command validates bounded manifests and streams read-only SHA-256 checks with before/after source signatures. It records errors in the new output directory and returns failure for missing or changed files. It never marks a decoder qualified. It does not import RAW into the editor or prove camera rendering quality, and refuses an existing output directory.
