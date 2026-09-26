# Bounded native demosaic parallelism

Status: implemented for one-pass Markesteijn and Bayer RCD on the shared Rayon pool. Native qualification and measured scope are recorded in [performance](../specs/performance.md#startup-and-raw-throughput).

## Execution boundary

`RawSource::develop` owns one planar output and calls `lf_raw_develop` synchronously from the bounded source worker. Normalization, demosaic, output scaling/finite checks and mandatory DNG corrections retain their order. The private Rust executor joins all native workers before border handling or before returning any failure. No borrowed pointer, callback or worker survives the call; no C++ exception or Rust panic crosses the ABI. Markesteijn's shared cube-root table uses once-safe initialization.

The pinned one-pass, non-CieLab algorithm retains its 114 px scratch tiles, 98 px stride, global origins, CFA phase, equations and per-pixel arithmetic order. Each ordinary full-height row is divided into consecutive jobs of at most eight full tiles, with the final two columns in one job to preserve right-edge scratch history. The final two rows share one original-order job and scratch allocation. Each job index is dispatched exactly once.

The Rust executor runs that final job on the source caller while at most seven ordinary jobs use the pool, then schedules remaining ordinary jobs in joined batches of at most eight. `in_place_scope` keeps an external caller's coordinator off the pool. A permit is held only during a callback and released before any join. One-worker execution runs jobs serially on its caller. The no-executor native fallback retains the original complete raster traversal. Small grids without at least three tile rows and columns, or unsupported algorithm settings, retain one serial job.

X-Trans dimensions below 120 px on either side fail explicitly: the pinned algorithm cannot initialize a complete first tile there. All currently configured X-Trans sensors exceed that minimum.

RCD retains its 194 px tiles, 176 px stride, equations and per-pixel arithmetic order. Each full-height tile row is divided into consecutive jobs of up to eight tiles; the row's partial right tiles stay in the job of their full predecessor. The final job runs from the last full-height row's last chunk to the end of the frame, so the partial bottom rows follow the tiles whose scratch they read. Every job is therefore a contiguous run of the serial raster that begins at a full tile. The same executor runs it: the final job on the source caller, ordinary jobs in joined batches on the pool. Frames below 194 px on either side, or without an executor, run one serial job over the original raster. Bayer dimensions below 10 px on either side fail explicitly: the pinned border pass would index outside the frame there. On the Z6 this gives 110 jobs and on the Air 2S 80.

## Why individual tiles cannot be independent

The pinned right-edge calculation reads scratch left by the preceding tile, and the bottom partial row reads scratch from its predecessor. Distributing every tile separately changed full-frame float bits at the right edge. Every full tile overwrites the RGB candidates, YUV/derivative buffers and reused homogeneity regions that its later stages read, independent of its column. Ordinary chunks can therefore begin with fresh scratch at any full tile. The rightmost pair and complete final two rows retain the required history without replaying work.

RCD has the same property for a different reason. A full tile reads only scratch cells it wrote itself or cells no tile ever writes, which stay zero. A partial tile also reads direction and colour-difference cells at its last computed row and column, which only an earlier, larger tile wrote. Distributing every RCD tile separately changed float bits on both Bayer files and on synthetic frames with partial tiles; the contiguous jobs above reproduce the serial raster exactly.

A tile row beginning at `t` retains `[t + min(t, 8), t + 106)` for a full tile. Its successor begins at `t + 98 + 8`, so retained interiors are disjoint. The same relation applies horizontally. The serial border pass runs only after every worker joins. Odd final tiles and real full-frame outputs are tested, including changed white balance.

## Why callbacks are bounded

A pool worker awaiting a preview can steal unrelated work. A callback that drained many row groups improved Fuji development but raised concurrent proxy p95 from 12.9 to 216.3 ms. One row per callback still left approximately 60 ms tails; moving the coordinator to the external caller and reducing native width to four did not resolve them. A trace located the slow fourth preview about 90 ms into a roughly 480 ms development, before the final bottom group. Whole ordinary rows were still too much stealable work.

Pool callbacks now contain at most eight ordinary tiles, for RCD as for Markesteijn; RCD's own effect on concurrent previews has not been measured. The longer bottom-edge job runs on the production bounded source thread, which already performs serial normalization and final conversion. No new thread or private pool is added. A library caller already running inside Rayon still executes that final job on its own Rayon thread; correctness/liveness is tested there, but the external-source contention result does not establish a latency guarantee for such callers. Scratch is allocated per callback and reused across its tiles; no frame allocation is added.

## Resource and lifetime bounds

At most eight native workers hold scratch permits across **all** callers. Each one-pass Markesteijn allocation is `(114² × 19 + 128) × 4 = 988,208` bytes, or 7,905,664 bytes for eight active workers; an RCD job's six tile buffers take `194² × 6.5 × 4 = 978,536` bytes under the same cap. Stack arrays, allocator overhead, row pointers and immutable native tables are additional; this is an explicit scratch bound, not a process RSS limit. One normalization frame and one planar output remain unchanged. Multiple independent callers retain their own full frames but share this scratch cap and the existing pool.

Each worker acquires one permit immediately before entering native code and releases it before any Rayon join. A permit must never be held by an outer scope waiting for children: nested Rayon work can otherwise deadlock under the global cap. A pool waiter cooperatively runs available work; only when idle does it park briefly. External waiters use a condition variable with a bounded cancellation check. These waits exist only while active callers exhaust the scratch slots; no idle poll is added.

Native workers check cancellation and shared error state between tiles, including within each job. Allocation failure, worker exceptions and cancellation join every callback, release scratch and discard the partial output.

## Qualification

The authentic Fuji test compares the complete float buffer against an immutable pre-change M4 digest, then compares serial with two, four and default-maximum workers, repeated and with changed WB. Synthetic odd shapes cover chunk boundaries, CFA phases and final edges against the no-executor traversal; sub-120 shapes fail deterministically. Concurrent calls, a small nested Rayon pool with all but one scratch slot held, unique job indices and bounded callback batches, peak slot accounting, cancellation within native work, injected allocation/exception errors and recovery exercise liveness and teardown.

For RCD, the authentic Z6 and Air 2S tests compare the complete serial float buffer against an immutable pre-change M4 digest, then with one, two, four and default-maximum workers and changed WB. Synthetic mosaics in all four CFA phases, from 10 px frames through skipped 18 px and 5 px final tiles to rows of several chunks, match the no-executor raster bit for bit at every worker count, with each job run once, on the caller or the pool, never more than eight at once. A counted cancellation proves a check before every tile and stops a frame after its first tiles with most samples unwritten; injected allocation and exception faults join and release their permits.

Nikon and corrected DJI native references, original preservation, sample/render parity, RAW editing/history/reopen and actual background desktop captures remain integration gates. Photo-sized release measurements include retained-mosaic development, cold saved-WB source preparation, shared-pool preview contention and process memory. M4 evidence does not qualify native Windows/Linux GPU or numerical behavior. SIMD/assembly and a GPU demosaicer remain separate measured candidates; no new scheduler or numerical approximation is introduced here.
