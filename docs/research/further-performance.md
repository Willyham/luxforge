# Further performance opportunities

Status: measured prioritization after startup overlap, in-place RAW conversion and bounded
Markesteijn execution. These are candidates, not implemented behavior or predicted speedups.
The [current measurements](../specs/performance.md#startup-and-raw-throughput) define the baseline.

## Highest-value contained candidates

| Candidate | Evidence and potential scope | Smallest useful experiment | Acceptance boundary |
| --- | --- | --- | --- |
| RAW Basic/Mixer row batching | The linear renderer's source-row shortcut requires no operations. A colour stack still resolves each pixel, rebuilds colour runs and invokes each unit on a one-pixel span. Dispatch savings are unmeasured. | Profile actual retained RAW planes with full Basic/HSL, then compare one bounded colour row using the existing unit math. Start with unmasked source+colour-only recipes. | Preserve source WB/exposure f64 order, the current f32 boundary, stage order, finite rejection and terminal bytes. A 16,384-pixel RGB float row is 192 KiB; no extra frame. Keep generic masks/geometry until separately proved. |
| Bayer RCD tile scheduling | Nikon retained-mosaic development is still about 269 ms. This whole-region cost is only an upper bound on potential saving. DJI also uses RCD before its already-parallel corrections. | Reuse the joined executor after proving tile scratch independence on complete Nikon/DJI float outputs and odd dimensions. | Preserve 194 px tiles, 176 px stride, 9 px borders, clamping, CFA phase and the final border pass. Its 978,536-byte scratch fits the existing 988,208-byte slot ceiling. Test contention, cancellation and native faults. |
| Remove RGBA publication copies | JPEG source adoption and fresh raster publication convert `Vec<u8>` into `Arc<[u8]>`, moving 96 MB at 24 MP or 240 MB at 60 MP per copy. Those byte counts are not elapsed-time savings. | Time the actual allocation/copy/free boundary, then prototype shared vector storage through core and the surface if material. | Retain capacity bounds, immutable ownership, exact bytes, source preservation and pointer identity through publication/upload. Account for simultaneous old/new frames. |
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

## Native scheduling and remaining serial phases

The contention diagnostic found a real cost when a preview waiter stole a long native callback.
One row group per callback substantially limits it, but a row still includes many tiles. Measure
finer scheduling or priority control against both Fuji throughput and proxy tails before another
scheduler change. Tile scratch history at the right/bottom edges forbids arbitrary independent
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
