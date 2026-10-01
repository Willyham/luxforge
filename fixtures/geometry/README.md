# Geometry reference fixtures

`lens-perspective.json` freezes independent f64 calculations from Lensfun v0.3.4
(commit `101c745e847a5de4a1e569a94368ce2027198598`) and the
[lens and perspective contract](../../docs/design/lens-and-perspective.md).
The reference crate imports no product code. XML record locations are recorded alongside each
calibration. The identity calibration is synthetic.

The fixture includes interpolation, normalization at 6048 × 4024, radial mapping and continuous
read bounds, separate-stage cover and local scale, the Perspective extremes and eight orientation
carry vectors. The alias-energy table reports RMS against 16 × 16 footprint integration of a
continuous checkerboard and zone plate at 32 × 32 output pixels; it is a numerical quality study,
not photographic qualification. The owner's 1.8× minification limit remains the admission rule.

Regenerate explicitly with:

```sh
cargo test -p luxforge-reference --test studies geometry::regenerate_committed_geometry_fixtures -- --ignored
```

Then run `cargo test -p luxforge-reference --test studies geometry::`. The ordinary test detects
stale committed values. Coordinate comparisons require 1e-9 px; product RAW tolerances are
1e-6 + 1e-6 × |reference|, and JPEG comparisons permit one code per channel. Identity and integer
paths, sample/full/region comparisons and serial/pool comparisons are exact.

This synthetic corpus does not qualify any camera or lens on real photographs. Missing
photographic evidence stays untested in the design's qualification matrix.
