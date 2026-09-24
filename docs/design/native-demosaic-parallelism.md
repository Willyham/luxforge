# Bounded native demosaic parallelism

Status: implementation proposal. Native development still runs serially. The first isolated performance wave changes terminal rendering and CPU colour/spatial kernels, not the demosaic scheduler.

## Boundary and expected benefit

`RawSource::develop` owns one planar output and calls `lw_raw_develop` synchronously from the source worker. The adapter normalizes the immutable mosaic, runs RCD or one-pass Markesteijn, scales and checks the output, then applies any mandatory DNG corrections. Those phases and their order stay the same.

A native M4 Pro release measurement of 30 warm developments from the supplied retained mosaics gave 267.6 / 270.9 ms p50 / p95 for Nikon Z6 and 1283.2 / 1295.7 ms for Fuji X100VI. Median process CPU time was 267.2 and 1281.4 ms respectively: approximately one core. In a separate five-second Fuji sample, 87.2% of flat samples were in Markesteijn and 9.9% in the adapter. Accelerating the entire sampled demosaic region fourfold would model about 444 ms total, but some work inside that function is serial. This is an opportunity estimate, not a measured parallel speedup or an editor latency claim.

The existing native build excludes OpenMP. RCD is also explicitly passed `multiThread=false`. Both vendor files already expose independent tile iteration under OpenMP pragmas, so the smallest policy-compatible implementation can retain those tile equations and supply shared-pool execution through the private adapter. Enabling OpenMP alone would add a second scheduler and requires a separate decision under the shared-pool rule.

## Proposed implementation

1. **Separate job preparation from tile evaluation inside the two pinned algorithms.** Retain CFA validation, immutable lookup tables, tile dimensions, global origin and output rectangles. RCD uses 194 px scratch tiles with 176 px strides and 9 px borders. Markesteijn uses 114 px scratch tiles with 98 px strides, then its existing final border pass. Do not invoke the whole demosaicer on arbitrary image crops: that changes CFA phase, tile anchoring and border equations.
2. **Expose a private synchronous executor callback through `lightwell-raw`.** The native call supplies a bounded tile count and opaque context plus a no-throw tile/worker function. A Rust trampoline runs a bounded number of worker slots on the existing global Rayon pool and joins every slot before returning to C++. No callback, raw pointer, source or output reference can outlive the call. The adapter is already the crate with the explicitly allowed unsafe FFI boundary; core keeps its forbid policy. This is a proposal for an internal signature, not a public API or module capability.
3. **Allocate one scratch set per admitted slot and reuse it across tiles.** An atomic next-tile counter or a fixed partition gives each tile exactly one owner; queued work carries indices, not image buffers. Choose the slot count from tile count, shared-pool width and an explicit scratch target. At most one slot may exceed a soft target, matching the host's existing budget behavior. Small frames use the serial path. Do not use `for_each_init` without accounting for extra task-local allocations beyond worker count.
4. **Join before the border and final conversion passes.** The source mosaic, normalization buffer, CFA and coefficient tables are read-only during tile work. Each worker writes only its tile's retained interior. Serial border handling runs after every worker has joined. Keep the per-pixel expression order, f32/f64 types and current clamping, including RCD's algorithm-specific nonnegative reconstruction.
5. **Make termination and initialization explicit.** Workers check the current cancellation token between tiles; after cancellation or any failure, the partially written output is discarded and never adopted. Catch C++ exceptions within the worker entry and translate them to the existing error classes; do not unwind across FFI in either direction. Replace unsynchronized process-global lazy initialization where needed: Markesteijn's `cielab` helper has a mutable static initialization flag, called even when `useCieLab=false`, so concurrent independent development calls need a once-safe initialization rule. Keep progress/error aggregation race-free even though the current adapter ignores progress.

## Memory and lifetime accounting

The current one-live-development gate, bounded source queue, encoded source, retained u16 mosaic, normalization frame and one planar output remain unchanged. No full-frame allocation is multiplied by worker count.

RCD's explicit scratch arrays total 6.5 × 194² floats, or 978,536 bytes per worker. One-pass Markesteijn's explicit tile allocation is `(114² × 19 + 128) × 4 = 988,208` bytes per worker. Eight such workers add about 7.5 MiB of tile scratch, not eight whole output frames. Thread stacks, allocator overhead and other native tables are additional and must be included in the measured ledger. These formulas describe explicit arrays, not a process RSS limit.

The FFI safety proof must cover immutable shared context, disjoint output rectangles including odd dimensions and final edge tiles, independent worker scratch, bounded indices, all callbacks joined before their captures are freed, and failure/cancel paths. Multiple independent `RawSource` callers must share the pool and scratch accounting without oversubscription or global-state races.

## Qualification and decision

Start with Markesteijn, where the measured absolute cost is largest, retaining a serial executor of the same tile function as the oracle. Compare complete float buffers byte-for-byte at worker counts 1, 2, 4 and the admitted maximum, including changed WB, odd edge tiles and repeated runs. Include Nikon and the corrected DJI path before exposing the same executor to RCD. Confirm unchanged source hashes, sample/display parity and full RAW editor reopen/history journeys.

Exercise concurrent independent developments, cancellation between tiles, worker allocation failure, propagated native errors and teardown. Measure source-worker responsiveness and competition with a proxy render, not just isolated demosaic throughput. Record a 30-sample release before/after on supplied photo-sized originals and peak scratch/RSS with both orders on the M4. Keep native Windows/Linux build checks distinct from native hardware timing.

This is a contained scheduler/FFI change but not a build-flag-only fix. The shared-pool version can proceed as its own implementation wave under the existing rules; adopting OpenMP, changing demosaic equations or choosing a GPU backend remains a separate decision.
