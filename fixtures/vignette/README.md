# Vignette fixtures

Synthetic-only, small, deterministic oracle fixtures for the post-crop [vignette
study](../../docs/design/vignette-study.md) of the [Presence, colour mixer and
vignette](../../docs/design/presence-mixer-vignette.md) design, which the production
`lightwell.vignette` module is checked against. Nothing here is a raster image; every case is a discrete `(pixel, parameters)`
sample computed by the independent `f64` reference in
`crates/lightwell-core/tests/reference/vignette.rs`, reloaded and checked bit-close against a
fresh computation on every `cargo test --package lightwell-core --test vignette_reference` run
(`committed_mask_case_fixture_matches_the_reference`,
`committed_amount_case_fixture_matches_the_reference`).

## Declared precision

Matches the study note's frozen tolerance: production mask and amount output must agree with
these `f64` values within `1e-6 + 1e-6 * abs(reference)` in linear light, with at most one output
code of difference at a quantization boundary. The fixture round trip itself (JSON text through a
parser back to `f64`) is checked at `1e-12` relative, four orders of magnitude tighter than the
production tolerance, so it only catches real staleness, not JSON's shortest-round-trip float
formatting.

## `mask-cases.json`

202 cases, each `{label, width, height, x, y, params: {amount, midpoint, roundness, feather},
expected_mask}`. `amount` is always `0` in this file (the amount equation is not exercised here);
`expected_mask` is `reference::vignette::mask(x, y, width, height, params)`.

Covers, at each of the three frozen aspect ratios (`24x16` = 3:2, `16x24` = 2:3, `20x20` = 1:1):

- **`defaults/`**: default parameters (midpoint 50, roundness 0, feather 50) at all four corners
  and the near-centre pixel.
- **`roundness/`**: roundness swept across `-100, -50, 0, 50, 100` (rounded rectangle through
  ellipse to true circle) at the default midpoint/feather, corner and near-centre.
- **`midpoint-feather/`**: a `7x7` grid of midpoint and feather (`0, 25, 50, 75, 90, 99, 100`) at
  roundness 0, corner only -- this is the grid the study's "corner saturation condition" is
  checked against (see the note): the corner reads mask `1` only while `r1 <= r_corner` for that
  frame size, and strictly below `1` once `r1` exceeds the discrete corner pixel's own radius.
- **`boundary/`**: the note's explicit worked example of that condition failing: `20x20`,
  midpoint 90, feather 90, corner mask `≈0.9942496570644717`, not `1`.
- **`midpoint100/`**: the midpoint = 100 edge case at feather `0, 50, 100`, where the falloff
  starts at (or past) the corner itself; see the note for what mask value each produces and why.

## `amount-cases.json`

60 cases, each `{label, width, height, x, y, input_linear_rgb, params, expected_mask,
expected_linear_rgb}`. `expected_linear_rgb` is `reference::vignette::apply(input_linear_rgb,
expected_mask, params.amount)`, so a loader can check `apply` directly without recomputing `mask`,
or check the full `vignette_pixel` pipeline since `expected_mask` is also recorded.

Covers, at each aspect ratio, amount in `-100, -50, 50, 100` (0 is checked directly by the
reference's own identity tests, not fixture-recorded), at the corner and near-centre pixel:

- **`flat/`**: a constant linear input, `[0.5, 0.3, 0.1]`.
- **`gradient/`**: a position-dependent input (`rgb = [x/(width-1), y/(height-1), average of
  those two]`), so the fixture also exercises a non-flat source, not only a uniform one.
- **`black-corners/`**: amount = -100 at all four corners of every aspect ratio on the flat
  input, the direct "corners go black" fixture (`expected_linear_rgb = [0, 0, 0]` in every case,
  since these corners all satisfy the saturation condition above at the default midpoint/feather).

## Hand-verified worked examples

Three of this file's cases are also independently hand-derived, with the arithmetic shown step by
step, in [the study note](../../docs/design/vignette-study.md#worked-examples) and asserted
directly (not just via the committed-fixture round trip) by
`hand_computed_example_one_ellipse_corner_at_defaults_is_one`,
`hand_computed_example_two_ellipse_near_centre_at_defaults_is_zero`,
`hand_computed_example_three_boundary_case_corner_mask_below_one` and
`hand_computed_example_four_amount_application_at_mask_one_and_mask_half` in
`crates/lightwell-core/tests/vignette_reference.rs`.

## What is not here

No production code reads these fixtures yet (the vignette module is a later task). No raster
images: every case is a scalar mask value or a single output pixel, not a rendered frame -- the
frame sizes here are chosen only to fix `width`/`height` for the coordinate formulas, not to
exercise tiling, streaming or any per-frame concern. No real-photo provenance, since these
fixtures are entirely synthetic. Regenerate both files with `cargo test --package lightwell-core
--test vignette_reference -- --ignored regenerate_committed_vignette_fixtures` after changing the
frozen equations in `crates/lightwell-core/tests/reference/vignette.rs`, and re-freeze the study
note to match.
