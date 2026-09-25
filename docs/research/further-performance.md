# Further performance opportunities

Status: measured prioritization after startup overlap, in-place RAW conversion and bounded
Markesteijn execution. These are candidates, not implemented behavior or predicted speedups.
The [current measurements](../specs/performance.md#startup-and-raw-throughput) define the baseline.

## Highest-value contained candidates

| Candidate | Evidence and potential scope | Smallest useful experiment | Acceptance boundary |
| --- | --- | --- | --- |
| RAW Basic/Mixer row batching | The linear renderer's source-row shortcut requires no operations. A colour stack still resolves each pixel, rebuilds colour runs and invokes each unit on a one-pixel span. Dispatch savings are unmeasured. | Profile actual retained RAW planes with full Basic/HSL, then compare one bounded colour row using the existing unit math. Start with unmasked source+colour-only recipes. | Preserve source WB/exposure f64 order, the current f32 boundary, stage order, finite rejection and terminal bytes. A 16,384-pixel RGB float row is 192 KiB; no extra frame. Keep generic masks/geometry until separately proved. |
| Bayer RCD tile scheduling | Nikon retained-mosaic development is still about 269 ms. This whole-region cost is only an upper bound on potential saving. DJI also uses RCD before its already-parallel corrections. | Reuse the joined executor after proving tile scratch independence on complete Nikon/DJI float outputs and odd dimensions. | Preserve 194 px tiles, 176 px stride, 9 px borders, clamping, CFA phase and the final border pass. Its 978,536-byte scratch fits the existing 988,208-byte slot ceiling. Test contention, cancellation and native faults. |
| Remove RGBA publication copies | The isolated publication boundary costs 1.49 / 1.79 ms p50/p95 at 24 MP and 3.72 / 3.82 ms at 60 MP. It also allocates another 96/240 MB transient buffer. | Prototype shared vector storage through core and the surface, then measure actual source preparation and frame publication. This is a smaller latency target than native development. | Retain capacity bounds, immutable ownership, exact bytes, source preservation and pointer identity through publication/upload. Account for simultaneous old/new frames. |
| Attribute the remaining startup floor | Argument parsing to editor initialization remains roughly 578–582 ms; the current first-image overlap does not reduce it. | Add monotonic owner/source/API/platform milestones to the initial event detail without changing event clocks. Compare stable background bundles separately from copied harness bundles. | Empty and first-image launches, first correct text/image, API readiness and failures must all remain correct. Defer API/session-file work only if its measured cost warrants it. |

The RAW colour candidate is in `crates/lightwell-core/src/render/linear.rs`: `source_rows`,
`pixel_in` and the terminal row renderer. The shared colour implementation is `color_runs` and
`apply_units` in `render.rs`. Existing 24/60 MP full-Basic numbers over generated JPEGs must not
be relabelled as actual RAW colour timing. Repeated Oklab conversions are operation math under the
current exact contract; apparent inverse transforms cannot simply be cancelled.

Large RGBA ownership changes cross `SourceImage`, `Raster` and the desktop surface, so they are
less isolated than removing the analogous encoded RAW copy. The encoded-copy experiment already
measured only 0.53 / 0.64 ms p50/p95 for Nikon's 30.6 MB, 1.39 / 1.61 ms for Fuji's 85.1 MB and
0.64 / 0.67 ms for DJI's 41.0 MB (30 samples, allocation/copy/old-vector free included; read,
input clone, equality and final drop excluded). That smaller change is low priority.

## RGBA publication measurement

Measured after the current wave was merged at `fd02c01`, on the M4 Pro/14-core/48 GiB host,
macOS 26.5.2, pinned Rust 1.94.0, standalone optimized build with `target-cpu=native`.
One-minute load was 2.24 before and 1.97 after. Thirty observations per size and variant ran in
15 ABBA blocks, including the first allocation and all tails. Inputs are deterministic 24/60 MP
RGBA byte buffers; this exercises the actual standard-library ownership conversion, not a whole
application load/render.

| Publication | 24 MP / 96 MB p50 / p95 | 60 MP / 240 MB p50 / p95 |
| --- | --- | --- |
| Existing `Arc<[u8]>::from(Vec<u8>)` | 1.486 / 1.794 ms | 3.717 / 3.821 ms |
| Prototype `Arc::new(Vec<u8>)` | Approximately zero | Approximately zero |

The vector-preserving variant is near timer resolution (both p95 values below 0.003 ms), so no
precise speedup ratio is claimed. Every result's complete bytes were validated outside timing.
`Arc<Vec<u8>>` retained the input data pointer in all 60 observations; slice adoption retained it
in none. The timed region includes adoption/allocation/copy and the old vector's release. Input
construction/clone and the result's final drop are excluded. The first slice observations were
5.20 and 11.72 ms, respectively; they remain in the distributions and are the maxima.

The actual copy boundaries are JPEG source publication (`open_source_bytes`), proxy publication,
new edited RGBA raster publication and terminal RAW raster publication. Identity rendering already
shares its source. App retention clones Arc pointers, and `queue.write_texture` is a separate GPU
upload copy. Changing `SourceImage`, `Raster` and `PhotoRaster` together could preserve the existing
vector allocation at CPU publication. It must account for retained capacity, not just length, and
prove exact bytes and pointer identity through the surface. No application implementation or
end-to-end speedup is established by this diagnostic.

Probe source, raw CSV, host snapshots, commands and analysis are retained locally in
`artifacts/performance-third-wave/rgba-research/`. The copy's measured region is the upper bound
on its isolated latency saving; decode, source-to-working-frame copies and GPU upload remain
separate. Keep this below RAW colour batching and Bayer development for latency prioritization,
while accounting for its larger transient-memory benefit.

## Native scheduling and remaining serial phases

The contention diagnostic found a real cost when a preview waiter stole a long native callback.
Bounded ordinary tile callbacks and moving the dependent final rows to the source caller keep
concurrent proxy p95 at 14.0 ms versus 13.3 ms before, with a 72% isolated development gain. Any
further scheduling change must retain both throughput and proxy tails. Tile scratch history at the right/bottom edges forbids arbitrary independent
tiles without a new proof. Do not exchange an isolated development win for hidden interaction stalls.

The native adapter still normalizes mosaics and performs final scaling/finite checks serially.
An older sample attributed 9.9% of Fuji CPU to the whole adapter, without isolating those passes;
that percentage cannot be applied to the new parallel wall time. Instrument each phase, then assess
periodic calibration hoisting or bounded row work with unchanged arithmetic and allocation counts.

## GPU, SIMD and assembly

These remain available options. A GPU pointwise preview could retain source textures and update
coefficients, but the current surface owns completed CPU output. First define ownership, upload,
readback, cancellation and the numerical contract; time input-to-presented-frame, not only the
shader. An approximate f32 shader cannot silently replace existing f64/byte-exact preview behavior.
A numerical tradeoff needs an explicit product decision.

SIMD or assembly should target an attributed hot loop with full-buffer references, architecture
fallbacks and photo-sized evidence. Inspect generated code before assuming hand-written instructions
beat the compiler. Exact output does not rule out acceleration; it makes numerical verification a
required part of the experiment.
