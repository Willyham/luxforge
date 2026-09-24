# Isolated rendering performance

The local CPU optimizations preserve photo output, editing semantics, recipes, API shapes,
scheduling and memory targets. Photo-sized costs and their measurement scope are recorded in
[performance](../specs/performance.md#isolated-rendering-kernels).

## Current kernels

**Presence.** `Plane::get` carries an ordinary inline hint. The release compiler can fold its
coordinate clamps and address arithmetic into the callers; the M4 comparison found no remaining
out-of-line calls to this accessor. Edge clamping, pixel arithmetic, f64 filter accumulation,
512 px tiles, parallelism and scratch bounds stay the same. The change adds no specialized
assembly or architecture-specific path.

**RAW terminal conversion.** Finite values are clamped at the existing terminal boundary and
quantized through the existing static sRGB code thresholds. Within `1e-12` linear of either
adjacent code boundary, the original forward power/round evaluation is retained: forward and
inverse floating-point transfer functions can disagree on the final byte at a boundary.
Non-finite rejection and the extended linear domain before terminal conversion are unchanged;
JPEG quantization keeps its own contract. The guard is conservatively tested on the M4, not a
formal cross-platform error bound for `powf`.

**Basic hue weighting.** The private skin-hue response accepts the bounded angle from atan2.
Subtracting its 55° centre gives `[-235°, 125°]`. The only part that would wrap lands in
`[125°, 180°]`, outside the ±35° active band either way, so the response uses the original delta
directly. This removes the unused general normalizer and its remainder without changing the
band test or cosine arithmetic. An independent periodic oracle checks the boundaries and a
million bounded angles; complete Basic/vibrance photo buffers match the previous implementation.

## Exactness and qualification

Independent filter references, serial/pool comparisons, tiles, masks and sample/render parity
cover Presence. RAW tests compare the original forward terminal conversion at all 255 code
boundaries and 128 f64 neighbours on either side, at the guard edges, through a dense sweep and
deterministic float bit patterns, and on signed, headroom and non-finite inputs. Complete
24/60 MP before/after output buffers provide a separate photo-sized check.

The isolated candidates use separate worktrees. Builds and tests may overlap; benchmark work is
serialized with all of those workloads paused. Each accepted source change receives targeted
checks and quick verification; integration covers rendered, timing and the native RAW journeys
with the local three-camera manifest. Measured kernel savings are not presented as desktop
latency or demosaic speedups. Native Windows/Linux numerical and desktop qualification remain
separate from the M4 evidence.

## Remaining opportunities

Native demosaic is still serial. The [bounded shared-pool proposal](native-demosaic-parallelism.md)
identifies the tile executor, scratch accounting, FFI lifetimes, cancellation and exactness
checks needed to change that without introducing a second scheduler.

Larger spatial tiles, changed point-sample scheduling, approximation tolerances, a GPU colour
backend and an additional native parallel runtime remain separate decisions. Larger tiles
reduce repeated halo work but increase the cost of a point sample on the catalog owner. These
local kernel changes do not accept that tradeoff.

## Performance review

| Question | Scope of these kernel changes |
| --- | --- |
| Original reads, hashes and decodes | No new request paths. Preparation continues through the signature-verified source cache. |
| Frame allocations and sharing | No new buffers. Existing output allocations, identity sharing and spatial/colour targets remain in force. The RAW quantizer reuses the existing static threshold table. |
| Point queries, validation and no-op checks | No new rasterization. Spatial sampling retains its declared one-tile exception and exact render parity. |
| Catalog owner work | No work moves to the owner; the existing spatial point-sample cost remains. |
| Desktop messages and uploads | No changes to state/history refresh, preview jobs or uploads. |
| Timers, polls and subscriptions | None added or changed. |
| Photo-sized cost | Release before/after evidence is recorded in the [performance specification](../specs/performance.md), with kernel diagnostics distinguished from `editor-performance` and native desktop journeys. |
| Exactness and sharing tests | Presence keeps the independent filter references, serial/pool, tile, mask and sample/render checks. RAW tests compare the original forward conversion at every code boundary and across finite/non-finite inputs. Hue tests compare the periodic formula at boundaries and through a million bounded angles. Full photo-buffer comparisons supplement these references; existing identity-sharing tests still apply. |
