# Source preparation and rendering performance

Status: implemented and qualified on the owner’s M4 Mac, with photo-sized exact comparisons and native editor evidence recorded in [performance](../specs/performance.md#source-preparation-and-exact-rendering). This batch preserves existing image and editing contracts; no approximation or GPU behavior is introduced.

## Scope

1. Cold known RAW sources develop once at the validated requested white balance, instead of as-shot followed by the saved gains. New imports retain as-shot behavior. Fingerprints, source interpretation, historical entry identity, concurrent changes and generation adoption remain authoritative.
2. Texture and Clarity declare that they need no global estimate. Dehaze's estimate identity excludes only its own strength; source development/view, upstream recipe and masks, sampling mode, stage, exposure and white-balance identities invalidate it. Existing provider defaults remain safe and caches bounded.
3. DNG stage-3 optical corrections use disjoint rows on the existing shared Rayon pool above a measured threshold. Preserve stage/channel order, f64 operations and tap summation, finite failures, cancellation and the single active-area scratch plane. No private pool or additional full-frame allocation.
4. Terminal quantization indexes the existing exact code thresholds through a small static coarse table, retaining tie/clamp behavior and the RAW canonical boundary guard.
5. Clipping reduction partitions disjoint output rows into one final grid for display-sized grids. Tiny grids with too few output rows use at most 64 fixed partial grids of at most 64 KiB each (at most 4 MiB total scratch) so the shared pool remains useful. Preserve exact cells, dimensions and resource limits. An additional persistent cache is outside this change.
6. A narrow source-only RAW render path resolves invariant layout once per row. Normal validation/compilation, all supported source orientations/crops, f64 source adjustments, terminal encoding, cancellation, metadata and generic fallback remain in place. Colour-unit batching is outside this change.

The research evidence is in local `artifacts/performance-next/`, against `3890a5b`. The source design contracts are [initial RAW](initial-raw.md), [Air 2S](air2s-dng.md), [Presence](presence-mixer-vignette.md), [instant previews](instant-preview.md) and [performance](../specs/performance.md).

## Constraints and open decisions

Originals remain immutable. Nothing moves pixel work onto the owner or UI thread. No new user-facing API, recipe or catalog shape is required; existing commands gain performance through the same implementation. Keep one active preparation/preview lane and existing resource limits. New parallel work shares the process pool and is measured under contention as well as in isolation.

GPU effects, approximate math, larger spatial tiles, viewport rendering, persistent previews, owner sample scheduling and startup preload remain proposals outside this batch. An optimization that cannot preserve its contract or beat noise is rejected rather than justified with a model. Windows/Linux numerical and native desktop qualification remain separate from M4 evidence.

## Acceptance and verification

Each worker uses an isolated worktree and commits only its implementation/tests. The coordinator owns common design/spec/status updates and integration. Targeted tests run during development; each completed agent change receives quick verification. Performance runs are serial, with all builds/tests paused. Save baseline binaries before implementation integration and record their hashes.

Exact numerical changes use independent canonical references, representable neighbours of all relevant thresholds, error/cancellation cases and complete photo-sized output comparisons. Source preparation exercises new import, current/historical saved WB, reload, interpretation/fingerprint mismatch and concurrent WB change. Spatial estimates prove actual preparation/reduction avoidance and correct invalidation. DNG compares serial/pool output and supplied-file samples. Overlay compares every cell against a serial oracle. RAW rows compare generic evaluation, samples and buffers across views and exposure.

Use 30 observations per reported distribution, retaining tails, cache state, host load and timing scope. Separate prepared-kernel time from decoder, UI and displayed-frame time. Alternate before/after legs for stable comparisons where possible. Verify integrated rendered/timing behavior and native RAW open/edit/reopen against the three-camera local manifest. Full verification subsumes rendered/timing when needed by integration; do not repeat passing tiers without a reason.

Current measurements and qualification limits are recorded in the performance specification and feature/user documentation. The completed task plan is removed. Initial verification failures and their focused reruns remain in local evidence; qualification combines those passing checks without repeating unchanged native scenarios.

## Performance review checklist

- Reads/hashes/decode remain on the signature-verified source worker; direct-at-target WB removes a redundant development.
- Quantizer adds a bounded static table; DNG retains one bounded scratch plane; clipping removes transient display-grid copies and bounds tiny-grid scratch to 4 MiB; RAW row path adds no frame-sized scratch.
- Samples remain exact and use the existing bounded spatial tile; skipping irrelevant estimates removes work without rasterizing a frame.
- Owner performs only catalog/identity/validation and job coordination; no new owner frame work.
- Desktop state/history/preview/upload paths and timers stay unchanged.
- Existing output sharing and resource limits are retained and checked by targeted tests.
- Photo-sized before/after measurements and native evidence are required before performance claims are recorded in the specification.
