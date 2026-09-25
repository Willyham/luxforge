# Further performance opportunities

Status: the guarded RAW colour-row path is implemented and measured on the owner's M4 Pro, 14 cores
and 48 GiB. Mask-paint phase attribution is implemented; its worker and surface costs are small
relative to queue and result-delivery tails, so no renderer-kernel change is justified yet. Other
items remain research opportunities. The
[current measurements](../specs/performance.md#startup-and-raw-throughput) remain the source for
accepted application baselines. Core renders, preview contention and whole-application measurements
have different scopes; do not add their savings.

## Ranked shortlist

| Rank | Opportunity | Evidence | Next work | Effort / risk |
| --- | --- | --- | --- | --- |
| 1 | Batch RAW Basic/Mixer colour rows | The production path reduces Z6/X100VI Full Basic p50 by 63–66% and Mixer by 43–45%; whole-buffer checks pass. Under continuous exact work, the Fit-proxy p95 falls 67% versus the generic renderer. See results below. | Keep eligibility narrow and correlate core gains with a presented generation when the hidden runner works; track the remaining contended proxy tail. | Small contained core change. Masks, geometry, replacements and spatial recipes retain the generic path. The core contention probe does not measure UI or GPU presentation. |
| 2 | Reduce mask-paint queue and delivery tails | On one bare masked layer, 30 positions at low host load yield input-to-presented p95 40.7 ms at 24 MP and 35.1 ms at 60 MP. Worker render p95 is 5.4/4.5 ms; queue wait is 18.2/17.4 ms and pre-result residual 20.3/22.4 ms. | Repeat with a phase sweep that separates active-job handoff from desktop event-loop delivery; preserve the one-active/one-pending bound and cancellation. | Diagnostic is complete. No compute-kernel rewrite is supported by the measured cost. |
| 3 | Parallelize Bayer RCD tiles | Whole native development is 268.6/270.7 ms p50/p95 for Z6 and 350.6/358.1 ms for DJI. RCD is serial, but its share is not isolated. | Prove disjoint interiors and independent scratch, then dispatch bounded tiles through the shared executor and compare whole mosaic outputs. | Medium-high. Preserve border/CFA behavior, cancellation and preview tail latency. No speedup estimate yet. |
| 4 | Attribute source-open and startup time | Existing copied-bundle launches are 764–809 ms p95 across empty, 24 and 60 MP cases; those totals include bundle copying and event polling. A phase probe reaches `Editor::new` about 142 ms after process entry but has not reached first view. | Restore a working hidden launch on this host; then separate bundle launch, app boot, read/hash/decode, source adoption, proxy raster and surface assignment on a stable bundle. | Small instrumentation, no optimization justified yet. Do not use the failed current launches as latency samples. |
| 5 | Measure GPU texture upload separately | The isolated `Vec<u8>` → `Arc<[u8]>` probe costs 1.49/1.79 ms p50/p95 at 24 MP and 3.72/3.82 ms at 60 MP, but production decode and render paths already allocate an `Arc<[u8]>` frame and write directly into it; `PhotoRaster` retains the same Arc. The remaining `queue.write_texture` is a distinct GPU transfer. | Keep the current CPU ownership path. Measure texture upload only if an end-to-end profile identifies it as material. | No publication API change is justified; broad ownership churn would not remove the separate GPU transfer. |

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

## Mask-paint phase attribution

The diagnostic now pairs only the scripted 30-position stroke with its preview generations, and
reports the four owner legs, queue wait, worker render, pre-result residual and surface assignment.
The latest one-run-per-size distributions on generated photo-sized fixtures are in the
[performance spec](../specs/performance.md#a-painted-strokes-own-latency). At leg start/end, load
was 3.91/3.91 for 24 MP and 3.46/3.46 for 60 MP; no build or test overlapped either run.

The gesture presents 26/30 positions at 24 MP and 24/30 at 60 MP. The input-to-presented p95 is
40.70/35.06 ms. Worker render p95 is 5.43/4.45 ms and surface assignment p95 is 0.04/0.02 ms.
Queue wait and pre-result residual are the larger terms, though the residual still combines the
interval before worker start with result delivery and does not name one hot function. Whole-launch
process CPU is 7.34/8.73 seconds; sampled peak RSS is 785.4/1288.2 MiB and includes GPU resources
and evidence captures. Scratch high-water is 15.12 MB in both runs. These CPU and RSS totals are not
per-preview costs or normal-open memory baselines. The 60 MP capture-heavy RSS does not establish a
production working-set regression.

The measured worker kernel and surface handoff are too small to justify SIMD, assembly or a GPU
colour-stage rewrite. Continue with a bounded scheduling/event-loop trace before changing queue
policy, and retain the current cancellation and memory bounds.

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

## RGBA ownership, GPU and SIMD choices

The production CPU publication boundaries already preserve one allocation. JPEG decode writes into
`render::zeroed_frame`; generic, linear RAW, spatial and proxy renderers write into the
Arc-backed frame they return. Identity renders retain the source Arc, and the app passes that same
pixel owner through `PhotoRaster` to the drawing surface. A broad `Arc<Vec<u8>>` conversion would
not remove a current production copy. The isolated 1.49/1.79 ms (96 MB) and 3.72/3.82 ms (240 MB)
`Vec`-to-`Arc<[u8]>` timings are only a standard-library boundary probe; their duplicate-byte counts
do not describe current application RSS. `queue.write_texture` remains a separate GPU transfer and
has no measurement here.

The smaller encoded-RAW adoption copy is measured at 0.53/0.64 ms p50/p95 for Z6, 1.39/1.61 ms for
X100VI and 0.64/0.67 ms for DJI. Keep it below the remaining startup and Bayer RCD work.

Do not start with assembly. The colour-row path is integrated; profile remaining operations and
inspect generated code before considering SIMD. Any vector path must preserve complete output bytes
on ARM64 and the portable fallback. A GPU colour preview needs an end-to-end measurement that
includes texture ownership, upload, cancellation and contention, with exact f64/byte behavior kept
for analysis, sampling, export and committed frames. Current evidence does not justify a GPU or
hand-written assembly speedup estimate.
