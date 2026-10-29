# Grain fixtures

Synthetic, deterministic oracle fixtures for the [grain study](../../docs/design/grain-study.md) of the [grain design](../../docs/design/grain.md), which the production `luxforge.grain` unit, in f32 Rust and in WGSL, is checked against. Nothing here is a raster image: every case is a hash, a compiled field or one pixel, computed by the independent `f64` reference in `crates/luxforge-reference/src/grain.rs` and reloaded against a fresh computation on every `cargo test -p luxforge-reference --test studies grain::` run (`the_hash_matches_its_frozen_test_vectors`, `committed_field_cases_match_the_reference`, `committed_pixel_cases_match_the_reference`).

Regenerate all three files with `cargo test -p luxforge-reference --test studies -- --ignored regenerate_committed_grain_fixtures` after changing the frozen equations, and re-freeze the study to match.

## Declared precision

Integers (hashes, mix outputs, component keys, steps) must match exactly. Lattice values are exact in f32 and f64 and must match exactly. Production field values must match `expected_field` within `1e-5` absolute, and production linear output must match `expected_linear_rgb` within `1e-5 + 1e-5 * abs(reference)`, with at most one output code at quantization: the study's frozen tolerance. The fixture round trip itself is checked at `1e-12` relative.

## `hash-vectors.json`

The hash for a WGSL or Rust transcription to check before anything else:

- `seed`: `SEED`, `0x4C584752`.
- `component_keys`: `mix(SEED + k)` for components 0 (fine), 1 (coarse) and 2 (clump): `0xa99c2dfc`, `0x80d15dec`, `0x4ac8a78e`.
- `mix`: nine `{input, output}` pairs of the lowbias32 finaliser, from `0` (which maps to `0`) through `0xffffffff`.
- `hash`: 21 `{component, i, j, hash, value}` cases, each component at lattice points `(0, 0)`, `(1, 0)`, `(0, 1)`, `(3, 5)`, `(65535, 65536)`, `(123456, 654321)` and `(4294967295, 4294967295)`, with `value` the lattice value `((h & 0xFFFF) + (h >> 16) + 1) / 65536 - 1`.

## `field-cases.json`

- `full_resolution_steps`: 101 triples `[fine, coarse, clump]`, the full-resolution steps at every integer Size from 0 to 100 (`round(65536 / (m * 6^(size/100)))`, `m` = 1, 2, 4). A production compile must equal them word for word. Size 0 is `[65536, 32768, 16384]`, the default Size 25 `[41874, 20937, 10468]`, Size 100 `[10923, 5461, 2731]`.
- `cases`: 45 compiled fields, each `{label, size, roughness, footprint: [fx, fy], compiled: {fine_step, coarse_step, clump_step, fine_gain, coarse_gain, clump_gain, variance}, samples: [{x, y, field}]}`, the field `G` (clamped) at six positions: `(0, 0)`, `(1, 0)`, `(0, 1)`, `(123, 45)`, `(5999, 3999)` and `(65543, 70000)`, the last past the 16-bit split of the lattice coordinate.
  - **`full/`**: footprint 1 at every Size and Roughness in {0, 25, 50, 75, 100} (25 cases); `variance` is 1.
  - **`reduced/`**: footprints 2, 4 and 8 at Size {0, 25, 100} and Roughness {0, 100} (18 cases): the reduced-stage rule's band-limited steps and attenuated gains.
  - **`fit/`**: a 6000 × 4000 output drawn at 1000 × 667, footprint `[6, 5.997…]`, at Size 25 and 100 and Roughness 50 (2 cases): a non-integer footprint with different steps on each axis.

## `pixel-cases.json`

91 cases, each `{label, amount, size, roughness, footprint, x, y, input_linear_rgb, expected_field, expected_linear_rgb}`, with `expected_linear_rgb` the reference's `grain_pixel`, so a loader can check the application alone (from `expected_field`) or the whole unit.

- **`flat/`**: greys at encoded 0.1, 0.3, 0.5, 0.7 and 0.9, Amount 100, Size 25, Roughness 50, at three positions. `flat/0.5/123x45` is the study's worked example: field `0.6993226`, output `0.2446771`.
- **`wedge/`**: an 11-step grey wedge from encoded 0 to 1 at Amount 50, one step a pixel along row 17; the ends, black and white, are unchanged.
- **`colour/`**: red, green, blue, yellow and a skin tone at Amount 100, at three positions; each output keeps its channel ratios.
- **`ends/`**: black, below black, near black (the additive rule), white, a channel at white, and three pixels with RAW headroom above white; every one but near black is unchanged.
- **`sliders/`**: Amount {0, 100} × Size {0, 100} × Roughness {0, 100} on encoded 0.5 grey at three positions; every Amount 0 case is unchanged.
- **`reduced/`**: footprints 2, 4 and 8 on encoded 0.5 grey at the default, the all-0 and the all-100 Size and Roughness, at two positions: the reduced-stage pairs a production reduced compile is checked against.

## What is not here

The reduced stage's statistical comparison against the area-averaged full-resolution grain is measured, not recorded: the study's tests render it on every full run, at the tolerance the study freezes. No raster images and no real-photo provenance: every case is synthetic.
