# Further performance opportunities

Status: the guarded RAW colour-row path is implemented and measured on the owner's M4 Pro, 14 cores
and 48 GiB. Other items remain research opportunities. The
[current measurements](../specs/performance.md#startup-and-raw-throughput) remain the source for
accepted application baselines. Core renders, preview contention and whole-application measurements
have different scopes; do not add their savings.

## Ranked shortlist

| Rank | Opportunity | Evidence | Next work | Effort / risk |
| --- | --- | --- | --- | --- |
| 1 | Batch RAW Basic/Mixer colour rows | The production path reduces Z6/X100VI Full Basic p50 by 63–66% and Mixer by 43–45%; whole-buffer checks pass. Under continuous exact work, the Fit-proxy p95 falls 67% versus the generic renderer. See results below. | Keep eligibility narrow and correlate core gains with a presented generation when the hidden runner works; track the remaining contended proxy tail. | Small contained core change. Masks, geometry, replacements and spatial recipes retain the generic path. The core contention probe does not measure UI or GPU presentation. |
| 2 | Find the cause of slow mask-paint frames | On the bare 24/60 MP recipe, p95 is 53–105 ms at low host load. The per-generation path is not split into queue, render, upload and redraw costs. | Add evidence-only phase timestamps to the existing generation correlation, then repeat on real 24/60 MP photos. | Small diagnostic, unknown fix. Instrumentation must not add work to ordinary strokes. |
| 3 | Parallelize Bayer RCD tiles | Whole native development is 268.6/270.7 ms p50/p95 for Z6 and 350.6/358.1 ms for DJI. RCD is serial, but its share is not isolated. | Prove disjoint interiors and independent scratch, then dispatch bounded tiles through the shared executor and compare whole mosaic outputs. | Medium-high. Preserve border/CFA behavior, cancellation and preview tail latency. No speedup estimate yet. |
| 4 | Attribute source-open and startup time | Existing copied-bundle launches are 764–809 ms p95 across empty, 24 and 60 MP cases; those totals include bundle copying and event polling. A phase probe reaches `Editor::new` about 142 ms after process entry but has not reached first view. | Restore a working hidden launch on this host; then separate bundle launch, app boot, read/hash/decode, source adoption, proxy raster and surface assignment on a stable bundle. | Small instrumentation, no optimization justified yet. Do not use the failed current launches as latency samples. |
| 5 | Preserve RGBA vectors at publication | An isolated ownership conversion costs 1.49/1.79 ms p50/p95 at 24 MP and 3.72/3.82 ms at 60 MP, with 96/240 MB of transient duplicate bytes inferred from the buffer sizes. | Introduce an immutable buffer owner through source, raster and surface APIs; confirm pointer identity at handoff and measure overlapping frame lifetimes. | Medium API change. Latency is modest; peak-memory benefit is promising but not yet an RSS measurement. |

## RAW colour-row implementation and measurement

The experiment used retained real RAW working planes from the Nikon Z6 NEF (4024 × 6048) and
Fujifilm X100VI RAF (7728 × 5152), not JPEG planes. A source-only colour segment previously resolved
the segment and its colour runs for every pixel. The production path batches eight rows at a time,
uses the shared Rayon pool above one megapixel, and calls the same source white-balance/exposure and
colour-unit math. It requires one identity-geometry segment with unmasked colour operations only;
masks, replacements, spatial stages and all other recipes keep the generic evaluator. Cancellation
is checked per row. Each Rayon folder reuses one width-sized RGB-float row buffer and accounts it in
the shared scratch target; if estimated pool-wide row scratch exceeds 64 MiB, the path runs serially.
Tests compare complete buffers with the generic evaluator for colour units, a viewed source, a
parallel frame with a partial final chunk, and masked/geometric fallbacks.

Release build, Rust 1.94.0, 30 observations per variant and recipe in 15 ABBA pairs. The reference
is the previous per-pixel evaluator with the same shared-pool scheduling threshold; the production
branch is the integrated row path. The timer includes output allocation and release, but excludes
RAW file read, decode/development and GPU or surface presentation. Process CPU is cumulative process
CPU per render, sampled outside each render timer, so it may exceed wall time on this 14-core host.

| Camera / recipe | Reference wall p50 / p95 | Production wall p50 / p95 | Reference CPU p50 / p95 | Production CPU p50 / p95 |
| --- | ---: | ---: | ---: | ---: |
| Z6 / Full Basic | 398.3 / 449.0 ms | 146.0 / 155.2 ms | 5217.9 / 5290.4 ms | 1888.0 / 1949.4 ms |
| Z6 / Mixer | 297.0 / 308.9 ms | 167.9 / 175.8 ms | 3866.0 / 3912.6 ms | 2154.4 / 2190.5 ms |
| X100VI / Full Basic | 646.4 / 893.3 ms | 220.2 / 290.1 ms | 7903.9 / 8149.5 ms | 2598.8 / 2738.2 ms |
| X100VI / Mixer | 503.9 / 607.2 ms | 278.0 / 315.2 ms | 5793.8 / 5914.1 ms | 3127.7 / 3236.8 ms |

The production median reduction is 63% on Z6 and 66% on X100VI for Full Basic, and 43% and 45%
for Mixer. These are independent recipe shapes, so their savings must not be summed into a combined-
stack estimate. They are core-render gains, not end-to-end input-to-presented-frame claims, and do
not overlap with native-development time elsewhere in the performance spec. The first Fuji pass was
less loaded at the tail: Full Basic reference/production p50/p95 was 618.8/700.3 and 201.2/252.7 ms;
Mixer was 496.8/557.0 and 277.1/315.2 ms. The table records the second 30-sample pass to expose the
tail variation rather than hide it.

At the start of the Z6 and X100VI runs, the one-minute host load was 6.38, 5.39 and 5.80 for the
repeat Fuji run. No build or test ran alongside the profiles; the benchmark itself drove the shared
pool and raised one-minute load to 14.80, 24.37 and 23.82. Per-render process CPU is about 13
core-equivalents for Full Basic and 11–13 for Mixer. The shared-pool Fit-proxy result follows; it is
core contention, not desktop presentation.

Retained source planes are 294 MB (Z6) and 491 MB (X100VI); the necessary RGBA output is 97 MB and
159 MB. Row scratch is 48,288 bytes per Rayon folder for Z6 and 92,736 for X100VI, or at most 676 KB
and 1.30 MB across 14 folders. Footprint after decode was 556 MB and 925 MB; after exactness,
reference and production snapshots were about 752 MB and 1.244 GB. The per-render production
snapshot did not exceed the generic snapshot. This is process footprint sampled around renders,
not an allocator trace or GPU allocation measurement; the exact-output check temporarily holds both
frames.

The narrow path is implemented. A separate Fuji stress test prepares a 1920 × 1280 nearest-sampled
proxy from retained RAW planes, then measures 30 Full Basic proxy renders while one external caller
repeats exact full-source renders through either the generic reference or production row path. This
deliberately keeps exact work continuously active, a harsher workload than the editor's
replaceable-preview lifecycle. It excludes decode, proxy preparation, desktop scheduling and GPU
presentation.

| Exact renderer sharing the pool | Proxy alone p50 / p95 | Proxy with exact work p50 / p95 | Exact renders | Overlap wall / process CPU |
| --- | ---: | ---: | ---: | ---: |
| Generic per-pixel reference | 13.9 / 26.5 ms | 623.9 / 658.3 ms | 30 | 18.8 s / 240.7 CPU s |
| Eight-row production path | 12.1 / 13.1 ms | 207.2 / 214.7 ms | 29 | 6.0 s / 79.9 CPU s |

The row path cuts contended proxy p95 by 67%, while its 215 ms result still exceeds the 32 ms
acceptable interaction bound. The process CPU counter covers both exact and proxy callers. Footprint
after overlap was 1.172 GB generic and 1.166 GB production; resident memory was 1.180 and 1.175 GB.
Leg-start load was 6.98 and 7.86, with no competing build or test. The intentional pool contention
raised ending load to 9.57 and 8.91. A one-row experiment produced 213.5/226.2 ms proxy p50/p95 and
completed 30 exact renders in 6.4 s, so it did not beat the eight-row path's 207.2/214.7 ms and 29
renders in 6.0 s. No GPU upload or presented-frame timing is available because the background app
runner currently stops before its first view event.

## Other measured editing opportunity

`editor-latency --mode paint` pairs each `mask_draft_set` with the displayed preview generation for
a paced sixteen-position brush stroke at Fit. The recipe contains one brush mask and one masked
Basic exposure layer. Six runs used generated 24/60 MP sources, warm filesystem cache and host load
3.90–5.67; this is an editor gesture measurement, not a core-only render.

| Source | p50 range | p95 range | Fastest observed |
| --- | ---: | ---: | ---: |
| 24 MP | 27.0–36.4 ms | 53.4–64.2 ms | 9.7–14.2 ms |
| 60 MP | 24.3–25.1 ms | 50.4–104.8 ms | 9.6–10.1 ms |

The test does not attribute the slow tail to brush compilation, preview queueing, rendering, upload
or a redraw wait; it also does not report process CPU or RSS. Add diagnostic-only stamps tied to the
existing generation ID, then repeat on photo fixtures with 30 samples per size. Preserve the current
single owner round trip, replaceable pending-preview bound and cancellation behavior while doing so.

## Startup and image-loading attribution

The existing 30-observation background measurements are app-cold and filesystem-warm. They launch
a temporary copied bundle with an invisible Metal window; the 10 ms event observer measures from
the outer launch to the first proxy event. Empty launches are 787.5/809.2 ms p50/p95, 24 MP opens
764.0/778.6 ms, and 60 MP opens 778.6/806.3 ms. The harness includes bundle creation/copy and
observer delay, excludes visible-window activation and scanout, and does not isolate CPU/RSS for each
startup phase. First-image overlap saved 42 ms at 24 MP and 98 ms at 60 MP; it did not change empty
startup.

The diagnostic phase trace recorded process entry to `Editor::new` at 141.7 ms. It did not yield a
complete startup distribution. On 25 September, both the instrumented release binary and a separate
release build of main timed out in the same hidden empty-smoke harness after the `startup` event and
before the first `view`, backend/Info or frame event. Both subprocess logs contain LaunchServices / XPC
connection warnings. The instrumented build had the same result, so this does not implicate its
bounded in-memory phase trace; it also does not establish the cause of the runner stall. These
failed runs provide no p50/p95, CPU or memory result. Rerun attribution when this background runner
can produce a frame again.

Before changing startup behavior, measure one stable app bundle and split file read/hash/decode,
source adoption, proxy creation, first raster and surface assignment. Keep the existing app-cold,
filesystem-warm copied-bundle series as a separate harness measurement. The current `open_to_raster`
clock has a different origin after the initial import began before platform setup, so do not compare
it with the older value.

## Bayer RCD and bounded scheduling

The current RCD adapter disables native multithreading and checks cancellation only after the native
call. Whole retained-mosaic development takes 268.6/270.7 ms p50/p95 on Z6 and 350.6/358.1 ms on the
DJI DNG. Those figures include output allocation/drop and other adapter work: RCD itself has not
been separately timed. They are upper bounds on the RCD opportunity, not predicted savings. CPU cost
and concurrent-preview contention were not measured for the RCD-only phase. The existing unchanged-
camera comparisons ran at leg-start load 3.2–6.5.

The RCD implementation processes 194 × 194 tiles at a 176 px stride, with 9 px overlap, followed by
a border pass. The tile interiors appear independent. A worker needs about 978,536 bytes of scratch;
eight admitted slots fit the existing 988,208-byte slot ceiling (7,905,664 bytes total). Keep the
existing shared pool and cap, use bounded tile callbacks and poll cancellation between tiles. First
prove exact full float outputs for Nikon and DJI, all CFA patterns, partial edge tiles and small/odd
dimensions; then measure it beside the Fit proxy workload. Preserve the serial final border pass
until its dependencies are separately proved. Do not confuse this with Fuji's X-Trans Markesteijn
path, which is already parallel and must retain its current preview-tail behavior.

The neighbouring Markesteijn work shows why this contention test is required: bounded tile callbacks
cut whole Fuji development wall time by about 72%, while process CPU rose about 19%, peak RSS rose
about 16 MB and concurrent Basic-proxy p95 moved from 13.28 to 14.01 ms. Less bounded schedules
raised the proxy p95 to roughly 60–216 ms. Reuse those admission and callback bounds; do not treat
idle CPU headroom as permission to occupy the shared pool with long callbacks.

## RGBA ownership and GPU/SIMD choices

The isolated M4 ownership probe used deterministic 24/60 MP buffers, 30 observations per variant in
15 ABBA pairs, and recorded load 2.24 before / 1.97 after. `Arc<[u8]>::from(Vec<u8>)` took 1.486/1.794
ms p50/p95 for 96 MB and 3.717/3.821 ms for 240 MB. `Arc<Vec<u8>>` adoption was below 0.003 ms p95
and retained the original pointer in all 60 observations; slice conversion did not. The timed region
includes allocation/copy and old-vector release, but excludes input construction, input clone,
complete-buffer comparison and final output drop. It is a standard-library boundary probe, not a
whole app import or GPU upload. The 96/240 MB transient duplicate is inferred from the byte lengths;
RSS and CPU time were not sampled separately.

The real boundaries are JPEG decode publication, proxy frames, new edited RGBA rasters and terminal
RAW rasters. Use a private immutable pixel-buffer type if retaining `Vec` capacity, then verify byte
identity and pointer continuity through `SourceImage`, `Raster`, `PhotoRaster` and surface upload.
Account for old/new frame overlap and retained capacity. `queue.write_texture` remains a separate
GPU upload and must not be counted as the same copy.

The smaller encoded-RAW adoption copy is already measured at 0.53/0.64 ms p50/p95 for Z6, 1.39/1.61
ms for X100VI and 0.64/0.67 ms for DJI. Keep that below the large RGBA ownership change in priority.

Do not start with assembly. The row path is integrated; profile the remaining hot operations and
inspect generated code next. Consider SIMD only if the compiler leaves a measured scalar bottleneck
and full byte-exact references still pass on ARM64 plus the portable fallback. A GPU colour preview
is a higher-cost follow-up: time the complete input-to-presented-proxy path including texture
ownership, upload, readback, cancellation and contention. Preserve exact f64/byte behavior for
analysis, sampling, export and committed frames; any approximate preview requires an explicit
numerical and product contract. Current evidence does not justify a GPU or hand-written assembly
speedup estimate.
