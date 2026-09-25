# Startup overlap and native RAW throughput

Status: implementation and qualification in progress. The owner authorized another measured performance pass, with implementation delegated to GPT-6 Sol subagents in isolated worktrees. The qualified previous batch is on main at `5d121d7`.

## Behavior and scope

1. Start the requested command-line image's existing bounded preparation job before platform/event-loop initialization. Reuse the desktop client and carry the job or error into the ordinary open/adopt path. Catalog identity, generations, cancellation, original verification, history and error presentation remain authoritative. Empty launches start no image job. No source pixels are read or processed on the UI or catalog owner.
2. Execute independent Markesteijn tiles on the existing shared Rayon pool through a synchronous private native callback, as specified in [bounded native demosaic parallelism](native-demosaic-parallelism.md). Keep CFA phase, global tile origins, equations, per-pixel order, border handling and one output frame. Bound concurrent scratch, join all callbacks, handle native failures/cancellation and remove unsafe lazy shared initialization. RCD remains serial unless separately measured and qualified.
3. Measure the core RAW camera matrix and repeated finite-validation scan. If material, execute exact disjoint chunks on the shared pool and adopt already-validated planes through a private boundary that retains dimension/capacity checks. Public constructors still reject non-finite input. Preserve cancellation and arithmetic order, with no extra full-frame allocation.

GPU preview, SIMD/assembly, JPEG decoding/copies, viewport rendering, finer spatial cancellation and scheduling remain research candidates. This batch does not relax numerical contracts, change modules or activate a new runtime dependency/scheduler. No unmeasured candidate is presented as a gain. Source timings, render kernels and first-frame timings are distinct and their savings overlap.

## Acceptance

- Native complete float buffers agree byte for byte with the serial reference at one, two, four and the admitted maximum workers, including custom WB, odd edge tiles and concurrent callers. Original hashes, RAW history/reopen and sample/render parity remain intact.
- Worker count and aggregate explicit scratch are bounded. Cancellation, failures and teardown publish no partial frame, leak no work and never unwind across FFI. No callback outlives the native call.
- Startup uses the same ordinary open behavior: missing/invalid files report normally, replacement cancels stale work, one source job is adopted once, existing saved white balance is respected, and headless/background behavior remains unchanged.
- Photo-sized release comparisons retain 30 samples per variant, tails, host load, hashes, exact scope and cache state. Serial timings wait for all builds/tests. Compare before/after in alternating legs where feasible; distinguish stable-bundle launch from copied harness launch.
- Run focused unit/integration tests while editing, quick once for finished code, then integrated background rendered/RAW evidence and relevant timing checks. Record initial failures and focused reruns honestly. Unsupported platforms and absolute targets under load remain unqualified.

## Resource and review checklist

Source work stays on the bounded worker. The native executor borrows immutable mosaic/context, has disjoint output interiors and reuses admitted per-worker tile scratch, then joins before borders. Core conversion mutates its existing three planes in place. Startup overlaps an existing job rather than creating a second service or cache. Pixel arithmetic, default previews, exact analysis and API/UI history have unchanged contracts. Final limits and measured outcomes belong in [performance](../specs/performance.md).
