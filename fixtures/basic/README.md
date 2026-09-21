# Basic and histogram fixtures

Synthetic-only, small, deterministic data prepared for the [Basic adjustments and
histogram](../../docs/design/basic-and-histogram.md) proposal ahead of any production
implementation (see that design's "Numerical and color contract", "Histogram and clipping
contract" and "Verification and acceptance" sections). Nothing here is generated from a real
photo; no private originals are committed. These fixtures are consumed by later tasks (a
histogram reducer, a float colour stage, an `Exposure` operation); as of this task no production
code reads them yet.

Expected values in every file were computed independently of any production implementation with
an inline `python3` command (not committed as a tool, per repository convention: private-photo
diagnostics and generator tooling stay out of version control, [fixtures policy](../README.md)).
Every expected array is a literal in its JSON file so a reader can inspect it directly; no value
here is computed at load time.

## Declared precision

- **Histogram counts and clipping predicates**: exact. They are integer reductions over exact
  output bytes, so every count and predicate in these files must match a correct implementation
  bit for bit.
- **The f64 colour reference** (`exposure-cases.json`, and the `reference` module under
  `crates/lightwell-core/tests/reference/`): exact to the stated formula (standard sRGB transfer
  constants: threshold 0.04045/0.0031308, 12.92, 1.055, 0.055, exponent 2.4; exposure
  `out = in * 2^EV`), computed in f64 with no intermediate clamping between colour operations.
- **Production tolerance** (for later tasks, not tested here): float colour output must agree
  with the f64 reference within `1e-6 + 1e-6 * abs(reference)` in linear light, and output codes
  may differ by at most one at a quantization boundary, per the design's numerical contract.

## Histogram fixtures

Each file holds `width`, `height`, `rgba` (flat bytes, opaque alpha 255), and the expected
`counts` (three 256-length integer arrays, `r`/`g`/`b`) and `clipping` object: `r0`, `g0`, `b0`,
`r255`, `g255`, `b255` (per-channel endpoint counts), `any_shadow`/`any_highlight` (pixels with
at least one channel at 0/255), `all_shadow`/`all_highlight` (pixels with all three channels at
0/255) and `both` (pixels that are both `any_shadow` and `any_highlight`). `name` and `note`
fields are documentation only; a loader should ignore unknown fields.

| File | Pixels | Distinct failure mode |
| --- | --- | --- |
| `black-4x4.json` | 4x4, all `(0,0,0)` | All-shadow endpoint population baseline |
| `white-4x4.json` | 4x4, all `(255,255,255)` | All-highlight endpoint population baseline |
| `primaries-complements-3x2.json` | 3x2: red, green, blue, cyan, magenta, yellow | Every RGB primary/complement; two-of-three-channel clipping, `both` predicate |
| `midgrey-128-5x3.json` | 5x3, all `(128,128,128)` | Uniform non-clipped population (every count/clipping field zero except the mid code) |
| `ramp-ascending-256x1.json` | 256x1, code `x` at column `x` | Every 0..255 code exactly once; only one all-shadow and one all-highlight pixel |
| `ramp-descending-256x1.json` | 256x1, code `255-x` at column `x` | Same population as the ascending ramp in reverse pixel order: proves order-independent, deterministic merging |
| `isolated-clip-2x2.json` | 2x2, one pixel with only its green channel at 255 | A single-channel clipped pixel isolated among unclipped mid-greys |
| `both-endpoint-2x2.json` | 2x2: `[0,255,0]`, a neutral pixel, all-black, all-white | Distinguishes `both`, `all_shadow` and `all_highlight` from each other and from the plain `any_*` counts in one buffer |
| `odd-7x3.json` | 7x3 = 21 pixels, no channel at 0 or 255 | Pixel count is not a multiple of 4, exercising remainder/stride handling in a reducer without mixing in a clipping case |
| `cropped-population-full-6x4.json` | 6x4, black/white border ring around an unclipped interior | The full composition before crop |
| `cropped-population-crop-4x2.json` | 4x2, the full buffer's central sub-rectangle (x=1..4, y=1..2) | The same interior pixels with the border's clipped pixels excluded: proves the histogram describes the composition after crop |

### Hand-verified by re-checking the arithmetic (not just re-running the script)

Three fixtures were independently re-checked by hand against the script's output, beyond writing
the generating formulas:

1. **`primaries-complements-3x2.json`**: each of the six pixels (red, green, blue, cyan, magenta,
   yellow) has exactly one or two channels at an endpoint. Counting by hand: every channel has
   exactly three pixels at 0 and three at 255 (e.g. R is 255 for red/magenta/yellow and 0 for
   green/blue/cyan), so `r0=g0=b0=3` and `r255=g255=b255=3`. Every one of the six pixels has at
   least one 0 channel and at least one 255 channel (a primary has one saturated channel and two
   zero channels; a complement has two saturated channels and one zero channel), so
   `any_shadow=any_highlight=both=6` and, since no pixel is entirely 0 or entirely 255,
   `all_shadow=all_highlight=0`. This matches the file exactly.
2. **`both-endpoint-2x2.json`**: pixels are `[0,255,0]` (both-endpoint), `[50,60,70]` (neutral),
   `[0,0,0]` (all-shadow) and `[255,255,255]` (all-highlight). By hand: `any_shadow=2` (the
   both-endpoint and all-shadow pixels have a 0 channel), `any_highlight=2` (the both-endpoint
   and all-highlight pixels have a 255 channel), `all_shadow=1`, `all_highlight=1`, and `both=1`
   (only the `[0,255,0]` pixel is simultaneously `any_shadow` and `any_highlight` — the
   all-shadow and all-highlight pixels are each only one of the two). Per-channel: `r0=2`
   (`[0,255,0]` and `[0,0,0]`), `g0=1` (`[0,0,0]` only), `b0=2`, `r255=1`, `g255=2`, `b255=1`.
   This matches the file exactly.
3. **`cropped-population-full-6x4.json`** and its paired **`cropped-population-crop-4x2.json`**:
   the full buffer has 16 border pixels (12 black, 4 white) and 8 unclipped interior pixels. By
   hand, the full file's clipping is `r0=g0=b0=12`, `r255=g255=b255=4`, `any_shadow=all_shadow=12`,
   `any_highlight=all_highlight=4`, `both=0` (black and white pixels are each only one of
   `all_shadow`/`all_highlight`, never both). The crop file is exactly the 8 interior pixels
   (`x=1..4, y=1..2` of the full buffer) with none of the border's black or white pixels: by
   hand its `counts` are the full file's interior contribution alone (each of the 8 interior
   values appears exactly once per channel, since all interior values are distinct and none is
   0 or 255), and every `clipping` field is 0. A later composition test can subtract the crop
   file's counts from the full file's counts and confirm the remainder is exactly the border's
   12 black + 4 white contribution.

## Colour reference cases: `exposure-cases.json`

A flat list of `{"input": [r,g,b], "ev": x, "expected": [r,g,b], "note": "..."}` entries (132
cases), computed with an inline `python3` f64 implementation of the same formulas, independent of
the Rust `reference` module. Covers: 0 EV identity on codes 0, 1, 127, 128, 254, 255; a 9-code
grey ramp (0, 32, 64, 96, 128, 160, 192, 224, 255) and the six saturated primaries/complements at
±1, ±2, ±0.5 and ±5 EV; and explicit output-boundary cases (200 at +1 EV clips to 255; 200 at
+0.5 EV stays below the clip at 233; 220 at +0.5 EV does clip to 255; 8 at -5 EV floors to output
code 0; 16 at -5 EV stays at output code 1 rather than flooring; 10 at -1 EV stays well below
1.0).

`crates/lightwell-core/tests/basic_reference.rs` loads this file and asserts the Rust
`reference` module's `evaluate_pixel` agrees with every `expected` value, cross-checking the two
independent implementations.

## Mixed-order composition cases: `mixed-order.json`

A 4x3 buffer with distinct pixel values, prepared for later mixed-order proofs (no production
evaluator for a mixed geometry/colour stack exists yet). Four cases, each an ordered pair of
operations and the resulting `expected_rgba`:

- `replace_then_expose`: a point replacement at `(1,1)` to `[10,20,30]` committed first, then
  +1 EV exposure applied to the whole image. The replaced pixel is exposed like every other
  pixel (its expected value equals `evaluate_pixel([10,20,30], [Exposure(1.0)])`, the same as the
  other base pixel that already held `[10,20,30]`).
- `expose_then_replace`: +1 EV exposure applied first, then the same point replacement committed
  last. The replacement pixel keeps its exact literal `[10,20,30]`, untouched by the earlier
  exposure, while every other pixel is exposed.
- `mirror_then_expose` and `expose_then_mirror`: a horizontal mirror and +1 EV exposure in both
  orders. Both produce byte-identical `expected_rgba` (asserted by the generating script before
  the file was written), since exact geometry is a pure permutation of pixel positions and so
  commutes with a pointwise colour operation.

## What is not here

No 24 MP or 60 MP workloads (those are generated by `cargo xtask generate-fixtures` into an
ignored directory, never committed). No bilinear resample reference (the crop spec already has
one; see [fixtures policy](../README.md)). No real-photo provenance or viewing-condition notes,
since these fixtures are entirely synthetic.
