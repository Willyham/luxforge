# Performance measurement plan

Status: provisional budgets, not accepted requirements. The owner's M4 MacBook Pro is the reference machine. The engineering rules every core and desktop change must follow are in [performance rules](../engineering/performance-rules.md). Fast interaction, background throughput and output correctness are evaluated separately.

## Reference workloads

| Workload | What it reveals |
| --- | --- |
| 24 MP and 60 MP JPEGs, EXIF-rotated variants, embedded sRGB/Adobe RGB/Display P3 profiles | First-open latency, memory, geometry and color |
| Huge or invalid dimensions, truncated files, malformed profiles | Resource bounds and error recovery |
| M4 with its actual display scale recorded; optional external SDR 4K | Preview, input, color, DPI and Metal resource measurements |
| Linux ARM64 VM, later native Windows/Linux GPU machines | Functional portability versus native GPU behavior, measured separately |
| 100,000 metadata rows; 1,000,000-row stress catalog | Index selection, pagination and startup independent of image bytes (catalog qualification) |
| At least 1,000 real images, then a larger owner dataset | Thumbnail decode and cache behavior synthetic rows cannot show |
| Local SSD, later removable SSD and NAS | CPU/GPU throughput versus storage latency |
| Nikon Z6 NEF, Fujifilm X100VI RAF and DJI Air 2S DNG in the owner's real modes | RAW decode/development, WB redevelopment, history, presentation and peak memory; see the [RAW integration contract](../design/raw-integration.md) |

Datasets need provenance, dimensions, profile and orientation, and redistribution permission. Synthetic fixtures live in the repository; private originals stay in a local manifest and are never committed.

## Provisional budgets

Engineering hypotheses until measured and accepted on the recorded M4 configuration, in SDR with the display contract recorded.

| Metric | Proposed budget |
| --- | --- |
| Launch to usable empty shell | p95 < 1 s warm, < 2 s cold |
| Uncached 24 MP JPEG to Fit preview | p95 < 750 ms; loading feedback within 100 ms |
| Crop overlay frame time | p95 ≤ 16.7 ms at 60 Hz |
| Slider input to presented frame at Fit, warm 24 MP | p95 < 16 ms; acceptable below 32 ms; a miss at or above 32 ms (owner's target of 2026-09-22) |
| RAW exposure drag at Fit, input to presented frame, Z6 and X100VI | p95 ≤ 50 ms |
| Burst drag (120 inputs per second for three seconds, alternating direction) | ≥ 30 presented frames per second; each presented frame's staleness p95 ≤ 50 ms |
| Settled exact histogram after the last input, 24 MP | p95 < 200 ms |
| Geometry input to presented preview | p95 < 50 ms once the source preview is ready |
| Empty steady-state process memory | ≤ 150 MiB including helper processes |
| 24 MP single-image edit working set | ≤ 600 MiB CPU-resident |
| 60 MP import or export peak | ≤ 1 GiB process RSS, GPU memory reported separately |
| Idle CPU | < 1% of one core over 30 s after background work settles |
| First page of a 100,000-row indexed filter | p95 < 100 ms warm (catalog qualification) |
| Warm adjacent-image Fit preview | p95 < 150 ms on a cache hit (catalog qualification) |

A single float32 RGBA buffer for 60 MP is about 916 MiB, so unrestricted full-resolution float processing needs tiling before it is promised.

## Auto tone

Native Apple M4 Pro (14 cores, 48 GiB) / Metal, optimized release, 2026-10-08, under the host-wide timing lock. This is not the owner's M4 MacBook Pro, and the host was shared with other agents' work: the one-minute load stayed between 3.90 and 7.96 during the run, below the 8.0 threshold, so the figures are recorded as reliable. Thirty samples per case use the prepared generated 24 MP and 60 MP JPEGs and a neutral Basic prefix. The engine timer covers the tile service's queue and grid read, then the solve after it off the tile service's thread (on the test's thread, as the analysis worker runs it; the hand-off itself is not timed); it excludes original decoding and preparation, catalog commit and preview. Cold and warm refer only to the analysis-sample cache; filesystem, device and shader caches are not purged. The model is Basic alone, or Basic then the starting Look at amount 200, which is not monotonic in Exposure and takes the coarse-to-fine search ([design](../design/auto-tone.md#the-solve-auto-tone1)).

| Source | Model | Renderer | Sample cache | Engine ms p50 / p95 | Grid read ms p95 | Solve ms p95 |
| --- | --- | --- | --- | ---: | ---: | ---: |
| 24mp | Basic | gpu | cold | 908.2 / 960.2 | 910.398 | 61.6 |
| 24mp | Basic | gpu | warm | 39.4 / 40.5 | 0.016 | 40.4 |
| 24mp | Basic | reference | cold | 50.6 / 54.4 | 11.427 | 43.1 |
| 24mp | Basic | reference | warm | 39.4 / 40.3 | 0.015 | 40.3 |
| 24mp | Look 200 | gpu | warm | 310.0 / 320.7 | 0.018 | 320.6 |
| 24mp | Look 200 | reference | warm | 309.5 / 328.4 | 0.017 | 328.4 |
| 60mp | Basic | gpu | cold | 2275.5 / 2342.6 | 2268.733 | 77.0 |
| 60mp | Basic | gpu | warm | 53.7 / 54.7 | 0.016 | 54.6 |
| 60mp | Basic | reference | cold | 63.8 / 70.8 | 10.136 | 60.9 |
| 60mp | Basic | reference | warm | 53.6 / 55.7 | 0.015 | 55.7 |
| 60mp | Look 200 | gpu | warm | 293.2 / 305.2 | 0.017 | 305.2 |
| 60mp | Look 200 | reference | warm | 292.8 / 322.8 | 0.018 | 322.7 |

On the same host, binary procedure and fixtures, the solver before the reduction and the analysis worker (solving on the tile thread, every search evaluating the whole sample) took 1,733.6 ms p95 warm at 24 MP and 1,566.6 ms at 60 MP through Basic alone, under a load of 1.79 to 6.28; through a Look above 100 it evaluated all 801 Exposure steps of the whole sample and was not measured. The committed values through Basic are identical before and after on both photographs, and through the Look at 200 they equal an exhaustive check of every Exposure, Whites and Blacks step on the same grids.

With the shared pool held to one thread (`RAYON_NUM_THREADS=1`), under a load of 1.84 to 5.45, the same binary's warm solve takes 220.9 ms p95 at 24 MP and 283.2 ms at 60 MP through Basic, and 2.13 s and 2.03 s through the Look at 200, with the same values in every case.

The 6000×4000 source uses 698,368 points and 9,078,784 retained bytes (8.66 MiB); the 10000×6000 source uses 628,736 points and 8,173,568 bytes (7.79 MiB). Both are within the 32 MiB retained cap. Warm GPU runs draw no tiles. The read's charged scratch peak is 20.06 MiB; the solver's charged scratch (`auto_tone::scratch_bytes`: the whole sample's and the reduction's `f64` luminances and one 48 KiB RGB chunk for each of the pool's 14 threads) is 6.24 / 5.71 MiB respectively. These scopes do not establish a total RSS/GPU memory budget. Both source hashes are unchanged.

The warm engine through Basic alone is within the provisional 250 ms / 400 ms click-to-entry budgets on this host with room for commit and preview, which are not measured here; through the Look above 100 the 24 MP solve alone exceeds 250 ms. The cold GPU grid read (0.91 s at 24 MP, 2.27 s at 60 MP) dominates a first Auto on a photograph. The owner's M4 click-to-entry qualification, reference spatial prefixes and non-neutral Looks at or below 100 remain unmeasured.

Evidence: `artifacts/auto-tone-measure-02.json` contains every sample, `artifacts/auto-tone-measure-02-one-thread.json` the one-thread run, `artifacts/auto-tone-measure-02-baseline.json` the solver before, and `artifacts/auto-tone-measure-02-host.json` records the commits, binary and source hashes, scope and the load sampled every 10 to 20 s through each run. Reproduce with the ignored release test `auto_tone_measure_photo_sized_inputs`, setting `LUXFORGE_GENERATED_FIXTURES` and a fresh `LUXFORGE_AUTO_TONE_OUTPUT`, on an idle native host under the timing lock. [Auto tone](../design/auto-tone.md) records correctness and the remaining corpus/owner qualification.

The separate standard timing tier last completed functionally with all four timing components marked unreliable (starting load 12.44–15.31); its figures are not a baseline.

## Code structure consolidation

The static `luxforge-gpu-types` / `luxforge-gpu` split and worker-scoped stream preparation are implemented ([design](../design/code-structure.md)). Native Apple M4 Pro / Metal, release, 2026-10-07: full verification passes all 77 components, including the complete available authentic-source corpus of 277 recipe/source pairs, with originals unchanged and existing tolerances retained. The four standard timing components are reliable, at one-minute starting load 3.69–4.67.

Matched measurements use separate release targets for pre-refactor `3ac13023` and integrated `d62f4aa0`, 30 samples per workload. Binary SHA-256 is `b3b913ddbd585015ebe32ea7077490513d08af0dc1c22fa70638aa1ee5eb9ea0` before and `c449fa464bf5466c0ced27a44184af566d76f8102ec24322d7c9a263239544df` after; the latter is also the full tier’s binary. Filesystem and system shader caches are not purged. Desktop drag preconditions warm for 1,000 ms, commit preconditions settle, and the export worker discards one warmup. Background invisible windows measure the editor’s texture, excluding scanout/compositing. High-load export observations are retained as unreliable; the accepted matched export trace is 3.18–3.60.

The controlled setup workload builds all three candidate grids plus staged/light selection from an already compiled evaluation, with no pixel reads or GPU work. Across JPEG and synthetic developed RAW at both sizes, plan preparations fall from five to one and stack compiles from five to zero. These are counts for the complete candidate workload; a production request may stop at an earlier strategy. Scoped worker tests independently prove one preparation and no recompile for the selected request.

| Setup domain/size | Setup ms p50 before → after | Setup ms p95 before → after |
| --- | ---: | ---: |
| JPEG 6000×4000 | 0.0615 → 0.0209 | 0.0664 → 0.0264 |
| RAW 6000×4000 | 0.0543 → 0.0180 | 0.0714 → 0.0182 |
| JPEG 9504×6336 | 0.0644 → 0.0265 | 0.0704 → 0.0295 |
| RAW 9504×6336 | 0.0665 → 0.0275 | 0.0693 → 0.0297 |

The editor stack holds Detail, a full non-neutral Basic layer and all three Presence fields. JPEGs are generated 24/60 MP fixtures; RAW is the authentic Nikon Z6 source. Each paired run preserves its original hash. Commit-to-frame can show the current processed motion frame before exact counts settle; those are separate costs.

| Source | Drag to frame ms p95 before → after | Commit to frame ms p95 before → after | Commit to exact histogram ms p95 before → after |
| --- | ---: | ---: | ---: |
| jpeg24 | 10.12 → 9.18 | 10.15 → 9.93 | 242.73 → 242.64 |
| jpeg60 | 10.55 → 9.54 | 16.67 → 16.59 | 643.74 → 649.69 |
| nikon-z6 | 9.47 → 9.34 | 10.49 → 10.48 | 309.74 → 310.10 |

The production export band stream is consumed in order with source development, JPEG encoding and durable publication outside its timer. The 20 MP masked RAW stream uses synthetic developed linear planes. Complete JPEG publication and source preservation are separately checked by the native corpus and rendered export journeys.

| Export-stream workload | ms p50/p95 before | ms p50/p95 after | Charged worker peak MiB, unchanged |
| --- | ---: | ---: | ---: |
| the 60 MP drag stack | 433.97/440.49 | 432.70/440.55 | 1212.43 |
| the Air 2S masked stack | 341.57/357.32 | 340.61/346.67 | 2023.05 |
| Detail alone at 24 MP | 91.29/93.92 | 92.95/98.86 | 622.29 |

All warmed stream samples compile zero GPU programs. Matched commit preview charge peaks are unchanged: 1242.41 MiB at 24 MP JPEG, 1953.81 MiB at 60 MP JPEG and 1963.08 MiB on Nikon RAW. These are existing charges, excluding crop/overlay, backend staging and driver/pipeline overhead; CPU scratch, sampled RSS and native GPU observations retain their separate scope. The measured editing, commit and export-stream costs remain similar; no generalized speedup or total-memory guarantee is claimed.

Linux aarch64 container headless checks with every graphics driver hidden pass, including slow tests and doctests, as does release owner acceptance. GPU tests skip explicitly there. The optimized editor builds and enumerates no adapter with drivers hidden; all 13 software-rendered journeys pass under Xvfb on Mesa llvmpipe (Vulkan, Cpu), including default reference rendering, explicit software adoption, histogram, GPU preview and no-GPU fallback. This is functional VM/software evidence, not native GPU evidence. Native Windows/Linux GPU evidence and authentic 60 MP RAW desktop/export timings remain unavailable; Windows CI is disabled. Those scopes and complete memory accounting remain open.

Evidence is in `artifacts/code-structure-integrated-full-corrected/summary.json` and `artifacts/code-structure-comparison/report.{md,json}`. The comparison retains every sample, matched workload/recipe, source/binary/lockfile identity, adapter, command, cache scope and host-load trace. Reproduce using the ignored `code_structure_stream_setup_measurement` and `code_structure_export_measurement` tests in release, plus `editor-latency --samples 30 --basic --detail --presence` in drag/commit modes on the same three sources. Use a quiet native host and fresh output directories.

## Window visibility and event-driven monitoring

The [visibility contract](../design/visibility-and-monitoring.md) pauses presentation sampling only
for native minimization or explicit window/app hiding. Visible unfocused or covered windows keep
sampling. Export, capability and catalog readers use bounded authoritative `job.wait` notifications;
background work and final/partial business answers remain active. Windows/Linux native visibility
facts are unsupported and retain ordinary sampling.

Native functional qualification on the Apple M4 Pro, Metal, macOS aarch64, 2026-10-06:
`verify --tier quick` passes, as do the touched `select`, `resolve-missing`, `develop-picks`,
`export`, `capabilities`, `performance` and `visibility-monitoring` rendered scenarios. Actual
AppKit order-out/order-in, minimize/restore and app hide/unhide callbacks correlate with timer/read
counts and captured state. Hidden resource reads stop, both long-work display timers are absent,
and restoring takes exactly one fresh sample with CPU/GPU rates unavailable until the second.
Photo pixels, geometry, recipes, disclosure preferences and original hashes stay unchanged.
Hidden export success/refusal and capability success/failure/cancellation are adopted. Catalog
first-look, folder-add, Search partial answers, Locate and Batch adoption retain job identity and
are tested independently of the activity board's display progress.

Measurement scope is the optimized `release` editor, binary SHA-256
`e9a4abbb29cb14245c96b4d6c59c7a0348afa68078e3b3e1bf534f16e00c2fd4`, Cargo.lock SHA-256
`d3fd725c365ad4eca4a24d6187e71dc5fc9f8f6678d04e603acb17a2f1624318`, and the generated
6000×4000 JPEG (source SHA-256
`b54c2a158a3d384674f5d731f940d553039d61f51b83a0e7b1e3b0247aa056eb`). The window is 1440×900
logical points in a background-only bundle: physically ordered in for native visible facts,
transparent and ignoring pointer input, never key or activated. Frames are renderer readbacks.
This qualifies the presentation gate and process cost in that harness; it does not qualify an
opaque foreground window's compositing or compare against a pre-change build.

Each run has three rounds of expanded, collapsed and hidden-expanded observations, 30 seconds
per state after a one-second settle. Evidence ticks and captures are suspended in each window.
External cumulative process CPU is read with `ps` every 500 ms; only readings entirely within the
app's declared window count, and the checks retain endpoints, actual elapsed time and excluded
edge gaps. `ps` has 10 ms CPU resolution: an unchanged counter means below that resolution, not
literal zero CPU. The external observer runs outside the editor. The internal observation still
includes its own start update/view and diagnostic boundaries; it is reported separately, rather
than treated as an observer-free CPU estimate. Evidence mode also retains the full update/derive
path for sampling, whereas an ordinary quiet editor has a sampling fast path. No kernel wakeup count is claimed: counters here
are application updates, board wakes and command requests/replies.

The held-job workload is three 20-second hidden observations while a local proof endpoint delays
an install. It uses the 480×320 orientation fixture, so only the separate idle workload above is
photo-sized evidence. Each held interval retains one waiter and changes neither job requests nor
replies, resource reads or board wakes; both display timers stay absent. Install completion and
restoration are checked after each interval.

Two runs, six observations per photo presentation state:

| State | Run | External CPU, % of one core (three rounds) | Contained elapsed span | Resource reads per 30 s |
| --- | --- | --- | --- | --- |
| Visible, expanded | 1 | 0.742, 0.740, 0.642 | 29.579–29.734 s | 30, 30, 30 |
| Visible, collapsed | 1 | Below 10 ms resolution, all three | 29.638–29.832 s | 0, 0, 0 |
| Hidden, expanded | 1 | Below resolution, below resolution, 0.034 | 29.262–29.649 s | 0, 0, 0 |
| Visible, expanded | 2 | 0.822, 1.183, 0.843 | 29.206–29.646 s | 30, 30, 30 |
| Visible, collapsed | 2 | Below resolution, 0.644, below resolution | 29.475–29.522 s | 0, 0, 0 |
| Hidden, expanded | 2 | Below resolution, below resolution, 0.034 | 29.030–29.574 s | 0, 0, 0 |

One 10 ms CPU step is about 0.034% over these contained photo windows. Every hidden window had
one full update and view from the observation start, no resource reads and no long-work display
timer. The ordinary visible expanded windows had 61 updates/views (two per sample plus the start),
and five collapsed windows had one. Run 2's adjacent second expanded/collapsed windows instead
had 93/47 updates/views, with resource cadence still 30/0, no job requests/replies or board wakes,
and stable native facts. Their extra UI activity is unattributed; keep their higher CPU figures
rather than treating those windows as quiescent or dropping them from the report.

| Hidden held install | External CPU, % of one core (three rounds) | Contained elapsed span | Requests/replies added |
| --- | --- | --- | --- |
| Run 1 | Below 10 ms resolution, all three | 19.155–19.662 s | 0 / 0 |
| Run 2 | Below resolution, 0.052, 0.051 | 19.215–19.548 s | 0 / 0 |

Each held window had one observation-start update/view, one held waiter, zero resource reads,
zero board wakes and no display timer. One CPU step is about 0.052% at this shorter duration.
Internal counters including diagnostic boundaries measured 5.7–10.6 ms for the quiescent hidden,
collapsed and held intervals. They are a different scope from the contained external readings.

Other builds and editor workloads ran on the host. Spot checks during the first photo run showed
one-minute load between about 5.9 and 28.1; the first capability run began around 38.3. A separate
five-second host-load trace for the second photo/capability runs recorded 4.43–18.87, and the
second held windows themselves were at 4.43–7.21. These are process CPU observations with native
functional evidence, without a generalized quiet-host baseline or a before/after speedup claim.
Rendering, proxy CPU work, preview/decode scheduling, CPU pools and memory targets are outside
this change.

Evidence: `/private/tmp/luxforge-visibility-monitoring-20261006-{1,2}` and
`/private/tmp/luxforge-visibility-rendered-capabilities-20261006-{1,2}` hold each run's result,
external process readings, correlated event/state/frames and `*-checks.json`, including raw
endpoints for every window. The host trace is
`/private/tmp/luxforge-visibility-host-load-20261006.jsonl`. Reproduce with
`cargo run --release --locked --package xtask -- smoke --scenario visibility-monitoring --output NEW_DIR`
and the `capabilities` scenario. The quick result is
`/private/tmp/luxforge-visibility-quick-20261006-4/summary.json`; its ignored/slow checks and timing
and full tiers are not claimed as passes.

## Detail and shared restoration: contract and review

Scope: the Restoration placement stage and `CompileStage`, Detail's bounded spatial units,
RGB16 JPEG hand-offs shared with Presence, exact-derived Fit display, the processed restoration
prefix cache, off-owner pixel reads and value-mask input grids. The [Detail design](../design/detail.md)
and [study](../design/detail-study.md) separate delivered code from outstanding image-quality gates.
The [GPU-first plan](../design/gpu-first.md#stages-and-what-each-deletes) has since removed the
restoration-prefix cache, Detail's own exact-derived Fit display, the prepared estimates, the source
proxy's windows and the quiet and settle policy; the answers below record the change as it was
reviewed, on that build, and those parts of them no longer describe the editor.
This section records the [performance-rules checklist](../engineering/performance-rules.md#review-checklist);
it contains no new timing, native residency or total-memory qualification.

| Review question | Answer for this change |
| --- | --- |
| Which paths read, hash or decode originals? | Preparation still uses the signature-verified source worker and decoded-source cache. Rendering, cached prefixes, deferred point reads and overlay grids receive immutable prepared sources. Cache keys hash bounded recipe/mask data, never source pixels. |
| Which full-frame allocations are new and how are they bounded/shared? | JPEG spatial hand-offs can be RGB16 at six bytes per pixel; each buffer is checked against 512 MiB and the uncached byte driver keeps at most two evaluated frames live. The one restoration-prefix entry added at most 128 MiB, independently of the one source proxy and eight prepared estimates; narrow RGBA8, RGB16 and RAW planar f32 prefix buffers were shared by `Arc` into suffix rendering. RAW spatial frames keep the 1.5 GiB per-buffer limit. Exact-derived Fit output was at most 8 MP of RGBA8 (32 MiB); reduce-only jobs share the retained full raster. One overlay input grid holds at most 8 million cells (six JPEG bytes or twelve RAW bytes per cell, plus an outside bitset), and its temporary grouped indices use four bytes per cell. Row scratch keeps the 64 MiB target; spatial work keeps the shared 256 MiB target and at least one finite oversized tile may progress. Overflowed working-set byte counts fail before allocation. These bounds do not define total process or GPU residency. |
| Do point queries, validation or no-op checks render a frame? | Validation and no-op planning read payloads and stage metadata. A pixel read through a spatial prefix is a GPU tile read by the tile service, or on the reference the prefix's spatial frames materialized once per call. Dense overlay cells share one tile pass; sparse cells evaluate grown windows through the same tile runner, over the frames of any earlier spatial segment. |
| What runs on the owner thread? | Descriptor/recipe validation, compilation, stage calculation, request identity, transactions and replay checks remain owner work. Every pixel read is handed to the tile service off the owner; a query's reads share one session, and a mutation parks once per pixel read, its replay checking the entry, revision, draft and verified source before committing. Preview rendering, cache construction, exact display reduction and overlay grid evaluation remain worker work. |
| Which desktop messages request state, history, previews or uploads? | Existing mutations keep their normal state/history refresh and one preview request. Fit resize, panel/display-scale changes and zoom-back request reduction alone when matching exact pixels exist; no additional `asset.state` or `history.list` call is introduced. The matching exact-derived display was uploaded once; it named displayed content without claiming full-resolution texture residency through `full_content`. Value-mask overlays rebuild only when their input/grid key changes. |
| Which timers, polls or subscriptions were added? | None. The event-driven workers, bounded latest-job queues and shared 25 ms quiet timer/120 ms settlement policy of that build remained. Detail checks cancellation between levels and bounded row chunks, including within the first tile. Disconnect drops queued and parked pixel reads and cancels the active one; obsolete reductions supersede on the preview worker. |
| Which unchanged work is cached, with which keys, limits and measurements? | The restoration prefix avoided repeated denoise/sharpen work during downstream drags; overlay and thumbnail input grids avoid repeated prefix evaluation when only mask coverage changes. Keys included source/development identity, canonical prefix and referenced masks, geometry, window, sampling dimensions, byte width and mask-input domain as applicable. Each worker kept one disposable derived-pixel entry: at most 128 MiB for the restoration prefix or 8 million cells for an input grid. Neither cache retained an evaluation or source. Exact recomputation, key invalidation and reuse are tested; photo-sized hit rates, rebuild costs and retained-byte measurements remain unqualified in TASK-014. |
| What did 24 MP before/after performance report? | Pending a quiet-host distribution. The tile kernels' before and after, at a one-minute load of 22 to 57, are in [Detail tile kernels](#detail-tile-kernels): a full 24 MP render's CPU time fell from about 19 to 6.3 s with the frame's bytes unchanged. No latency-budget pass, Presence JPEG regression verdict or total-memory claim follows from the focused functional tests. Native measurements must record build/source identity, host load, sample counts, warm/cold prefix use, source-proxy construction and the extra exact-derived display reduction. |
| Which tests prove exactness and sharing? | Detail's independent reference and deterministic production oracle cover coefficients, extended/constant pixels, tile sizes, sampled kernels and serial/pool parity. RGB16 tests covered every threshold, original 8-bit decode identity, the dark smoothing ramp through Basic, full/sample/region/window equality and buffer limits. Restoration-cache tests compared cached suffix bytes with uncached JPEG/RAW output, whole and cut-window stages, masks, boundary-width changes, downstream reuse and key invalidation. Worker/app tests checked exact-derived Fit reduction, generation/content adoption, retained-raster sharing and the distinction between displayed and full-resolution content. Deferred-read and input-grid tests check exact prefix values, owner responsiveness, revision fences, dense/sparse equality and cache invalidation. Native rendered/photo qualification and measurement-only tests remain separate evidence. |

### Detail tile kernels

Detail's smoothing runs over whole rows, one tap at a time across the row, the interior through
slices of it and only the columns within a kernel's radius of the stage's edge through the clamped
read, with the halo checked once a pass. A level of noise reduction shrinks all three channels in one
pass, taking each pixel's chroma energy and factor once; it smooths only the channels whose threshold
there or at a later level is not zero, so Luminance alone smooths no chroma and the fourth level no
lightness; it takes the band in the vertical pass and swaps its planes rather than copying them back.
Sharpening whose blur is its guide, as at Radius 1, smooths once ([Detail](../design/detail.md)).
Each pixel's operations are the ones before, in the same order, so the output is the same to the bit:
the tap-by-tap references frozen in `modules/detail/exactness.rs` and every earlier Detail test hold
it, and `detail-performance`'s `frame_sha256` did not change.

`detail-performance`, release, 30 samples a run, its fixed recipe (Detail at Luminance 25, Colour 25
and sharpening 40 under a +0.5 EV exposure), the builds before and after interleaved, Apple M4 Pro,
2026-10-03, at a one-minute load of 22 to 57: well past the 8.0 a quotable distribution needs, so the
wall-clock figures are not quotable, and the CPU time each render took is the figure that holds. The
textured source is `detail.jpg` tiled to 6000 × 4000, since the generated 24 MP JPEG is flat
quadrants.

| Source | Full render, CPU s p50, before | After | A point through Detail, first request / first colour-limited tick, ms p50, before | After |
| --- | ---: | ---: | ---: | ---: |
| `24mp.jpg` | 19.1, 19.8 | 6.39, 6.33 | 129 to 137 / 130 to 146 | 27.4 to 27.5 / 27.4 |
| Tiled `detail.jpg` | 19.6, 18.1 | 6.21, 6.23 | 137 to 139 / 144 to 175 | 28.4 to 35.5 / 28.5 to 36.6 |

One 512 px tile, serial, in one release binary holding the frozen references, at a load of about 28,
ms p50: noise reduction at Luminance and Colour 25 took 14.2 against 118, Luminance 40 alone 7.8
against 60, Colour 40 alone 11.8 against 105 and the first recipe at a proxy's scale 17.9 against 164;
sharpening at Radius 1 took 14.7 against 23.6, and at Radius 2.3 14.1 against 24.6. The largest cost
left is the sharpening limiter's 3 × 3 extrema and gradient, read through the clamped per-pixel index,
about half of sharpening, and the same index in the Oklab conversion and the reconstruction.

```sh
cargo run --release --locked --package xtask -- detail-performance --source fixtures/generated/24mp.jpg --output NEW_DIR --samples 30 --case render
cargo run --release --locked --package xtask -- detail-performance --source fixtures/generated/24mp.jpg --output NEW_DIR --samples 30 --case points
cargo test -p luxforge-core --lib detail::exactness
```

### The 16-bit hand-off's quantizer and Vibrance's hue

The JPEG path's 16-bit hand-offs, a colour segment feeding a spatial layer and every spatial tile's
output, quantize each channel through an exact index (`Quantizer16`): 65,536 bins over the value's
square root, where the code thresholds lie at least 1.055 × 10⁻⁵ apart against a 1.526 × 10⁻⁵ bin,
so a value is compared with at most two thresholds, where it searched all 65,535. The index is 128
KiB, built once ([limits](../design/architecture.md#limits)), and gives the search's code for every
`f32` in [0, 1], NaN, the infinities and the signed zeros
(`slow_byte_quantize16_is_the_threshold_search_at_every_f32_in_the_unit_interval`). Vibrance skips
the hue's `atan2` where Oklab's (a, b) lies more than a degree outside the skin-tone band, where the
hue's weight is exactly one, so every weight is the same to the bit
(`skipping_the_hue_outside_the_skin_band_leaves_every_weight_bit_identical`). Release, one thread,
both kernels in one process alternating, 11 samples, 3 October 2026, a one-minute load of about 6:

| Kernel, source | Before | After |
| --- | ---: | ---: |
| 16-bit quantizer, gradient (ns a value) | 11.71 | 1.34 |
| 16-bit quantizer, `24mp.jpg` | 10.10 | 0.66 |
| 16-bit quantizer, `detail.jpg` | 10.03 | 0.85 |
| ColourAdjust at Vibrance 50 and Saturation 20, gradient, 85% skipped (ns a pixel) | 24.43 | 22.48 |
| The same, `24mp.jpg`, 50% skipped | 24.51 | 23.72 |
| The same, `detail.jpg`, 12% skipped | 19.54 | 19.16 |

A 24 MP JPEG render of Basic, then Presence's Texture, then the Mixer, whose two hand-offs are
16-bit, took 334 and 405 ms against 512 and 526 ms (p50, base, new, new, base, at a load of 5.5 to
11, the same SHA-256 each way).

### The integrated before and after, 3 October 2026

The Detail tile kernels, the 16-bit quantizer, Vibrance's hue, the masked colour skip and the
linear resample's gate ([Detail tile kernels](#detail-tile-kernels),
[the quantizer](#the-16-bit-hand-offs-quantizer-and-vibrances-hue),
[masked colour](#units-only-where-the-coverage-is-not-zero),
[per-pass thresholds](#per-pass-parallel-thresholds)) together: release `xtask` built from
`ce3b7b57` (before) and from the working tree holding them (after), run before, after, after,
before on the native Apple M4 Pro, each run holding the host-wide timing lock, at a one-minute
load of 6.9 to 13.4. That is past the 8.0 a quotable baseline needs, so these are paired
comparisons, not baselines.

- **`detail-performance --case render`, 24 MP, 30 samples.** A full render took 1,225 and 1,224 ms
  p50 before and 321 and 321 ms after (p95 1,370 and 1,266 against 340 and 391), about 15.8 s of CPU
  a render before and 4.0 s after; `frame_sha256` was `e64d4d51…` in all four runs.
- **`editor-performance`, 30 samples, 24 MP and 60 MP.** No row moved past the runs' own spread but
  the Vibrance and Saturation layer: 42.9 and 44.3 ms against 44.6 and 46.5 at 24 MP, and 95.9 and
  98.5 against 99.5 and 101.7 at 60 MP (p50), the hue's skip. Its stacks hold no mask, Detail layer or
  RAW, which the other changes speed.

### Detail RAW residency diagnostic

The background native RAW journeys on the M4 Pro/Metal at 2x density expose a memory investigation
case. These are ten functional captures per supplied photograph, with Detail, white balance and
geometry changes, from release editor SHA-256
`3a89cb5db2709acef271ca0f0beea15111543cafaa1b09b4e73f84ad87af2a9a`.
All three final-build functional journeys pass. Surrounding host-load observations exceed the
8.0 quiet-host threshold; these are not performance distributions or residency qualification.
State resources include the process's source,
developments, caches, retained previews and unified GPU allocations; GPU bytes are not added again.

| Supplied source | OS footprint high-water MiB | Maximum captured resident MiB | Maximum captured reported GPU MiB |
| --- | --- | --- | --- |
| Nikon Z6, 4024 × 6048 | 2583.41 | 1909.75 | 348.53 |
| Fujifilm X100VI, 7728 × 5152 | 3301.11 | 2615.39 | 454.14 |
| DJI Air 2S, 5464 × 3640 | 2339.78 | 1820.70 | 319.03 |

The resident column is a maximum over captured resource samples, not a measured process RSS peak.
Even these samples exceed the 1536 MiB RAW investigation target on each source. TASK-014 remains
open for attribution, idle/cache release, transient/backend staging and finished-build measurements;
the per-buffer and scratch bounds above do not constitute a total-memory pass. Correlated states
and captures are in `artifacts/detail-20260930/native-raw-{nikon,fuji,dji}-final/app/`.

## Recorded baselines

Native M4 Pro, release builds, warm filesystem cache, synthetic fixtures. Diagnostic observations, not accepted budgets or cross-platform claims.

### Sample counts for a p50/p95 claim

Every harness command's default run is a functional run: it proves the journey and gives one launch count you can quote, not a distribution. A p50/p95 figure requires an explicit sample count: 30 samples per recipe for `editor-performance`, 30 inputs for `editor-latency` (one launch), and at least 5 launches per workload for `measure` — 5 gives a median and a maximum, not a stable p95, so use 30 launches per workload for a p95 claim. Every recorded figure states the count it was taken with. Every p50 and p95, in `xtask`'s timing tools and in the crates' own ignored timing tests alike, is now read from one nearest-rank `Distribution` (`luxforge-testbase`): `sorted[ceil(percent·n/100) − 1]`, always one of the samples, never interpolated. Figures recorded below before that change were taken with the tool's or test's own definition and can read one rank apart from the same measurement taken now. The `measure` medians and p95 figures used an interpolated median and an uncapped `p95 = sorted[n·95/100]` index, so an even-count median differs most. The crates' timing tests mostly used the upper median `sorted[n/2]` (or `sorted[round((n−1)/2)]`), which at an even count is one rank above nearest-rank's p50: `presence_timing` and `spatial_timing` at 10 runs, `masked_spatial_zero_coverage_timing` at 6, `capability_timing` at 30 and 200, the core's shared-pool Fit-proxy contention figures at 30 (theirs was `sorted[ceil((n−1)·q)]`), `resources_cost` and the process sampler's `cost` at their even counts, and the preset inspection timing at 20. Their p95s, and every figure at an odd count, are unchanged.

| Measurement | Result |
| --- | --- |
| S0 viewer launch to observed frame (empty / 24 MP / 60 MP) | median 233 / 296 / 412 ms |
| S0 request to captured frame (24 / 60 MP) | median 237 / 361 ms |
| S0 sampled peak RSS (empty / 24 / 60 MP) | 111 / 506 / 772 MiB; 965 MiB after sixteen 60 MP loads |
| S0 idle CPU after settling | 0.033% of one core over 30 s |
| Core import of a 24 / 60 MP JPEG | 55 / 118 ms |
| Pixel edit after a rotate on 24 MP | 0.2 ms (sampling path) |
| Core one transform on 24 MP after the module registry (p50 / p95, 20 samples) | 12.2 / 13.5 ms; 200 composed transforms 11.4 / 11.9 ms; registration of the built-in modules 0.18 ms and first render after open 0.05 ms on the 480×320 fixture (release acceptance run) |
| Editor RSS after M1/M2 journey with a small fixture | about 101 MiB, 0.2% CPU idle |

### Host and build for the current JPEG editor baseline

Native Apple M4 Pro (14 cores), 48 GiB RAM, macOS 26.5.2 (25F84), Metal on the `Apple M4 Pro`
adapter, a 2880 × 1800 physical window at 2× scale; files on the internal APFS SSD. Release builds,
`--locked`, background-only launches, warm filesystem cache without an OS cache purge. Application
SHA-256 `2960abd8bcc922b43db7170e8579ff60df56a1177f86d8658e332dbb649239ed`, Cargo.lock SHA-256
`e1f96098ab03786e8976afd4e0ed78b0ed3d4c58cd071c032390552c97b4a600`. Fixtures are
`fixtures/generated/24mp.jpg` (6000 × 4000, SHA-256 `b54c2a15…`) and `fixtures/generated/60mp.jpg`
(10000 × 6000, SHA-256 `b9e0118a…`). The host is shared with other sessions: the one-minute load
average was between 2.3 and 5.7 at the start of every run recorded below, and the `editor-performance`
runs themselves drive the shared Rayon pool across all cores, so these are diagnostic samples on a
live machine, not a quiesced benchmark.

### Core render, one run, 30 samples per recipe

`editor-performance` on 24 and 60 MP, 30 samples each, release, warm cache. Every row below comes
from **one** invocation per size, so the rows are directly comparable to each other; earlier
per-task rows measured in separate runs are superseded (see the note after the table). This is
core request-to-render on the catalog owner's thread only: no desktop scheduling, GPU upload or
presentation. The colour rows all render the same 200-transform-and-10°-crop stack with one Basic
layer inserted where the host places a colour-stage commit, compiled by the real `luxforge.basic`
module, so the difference between a colour row and the baseline row is that unit's own per-pixel
work on identical frames.

| Measurement (p50 / p95 ms) | 24 MP | 60 MP |
| --- | --- | --- |
| Identity recipe (shared source buffer, no frame allocated) | under 0.01 / 0.01 | under 0.01 / 0.01 |
| One exact transform | 10.9 / 15.0 | 24.6 / 38.5 |
| 200 transform actions folded into one orientation layer | 10.7 / 12.7 | 22.1 / 26.1 |
| The same stack with a 10° `crop-fit` on top | 33.7 / 68.9 | 69.7 / 85.4 |
| Colour baseline: the same stack, no colour layer | 32.8 / 41.4 | 69.7 / 74.6 |
| … with one `+1 EV` Basic layer (Exposure) | 48.3 / 55.9 | 111.9 / 120.7 |
| … with `+1 EV` and all five Tone fields | 80.8 / 105.2 | 196.4 / 210.8 |
| … with `vibrance 50, saturation 20` (the fused `ColourAdjust` unit) | 106.0 / 117.3 | 273.6 / 289.9 |
| … with `temperature 30, tint −10` (white balance) | 53.4 / 70.3 | 128.0 / 136.4 |
| `analysis::reduce` alone, over an already-rendered raster | 6.9 / 7.7 | 17.4 / 27.5 |
| `query.neutral-sample`: the whole picker, 25 point samples | 0.01 / 0.03 | 0.02 / 0.03 |
| `crop-fit` commit: validation, fitting, compile and persistence, no render | 0.79 (single) | 0.85 (single) |
| Import | 54.4 (single) | 127.3 (single) |
| Reopen: source and preview job after a fresh `EditorService` | 30.5 (single) | 89.9 (single) |

The reopen row times the catalog owner's own preparation, run blocking by `EditorService::prepare` —
the source job's read, hash and decode and the owner's completion — followed by the preview job. The
figures above were taken while the harness read the original through a synchronous shortcut the
product never took; they are re-measured at the next timing run.

The crop output stage measures 3695 × 2077 from the rotated 4000 × 6000 input at 24 MP and
5542 × 3116 from 6000 × 10000 at 60 MP. Against the colour baseline on the same run, each unit's own
added cost at the median is about 15 ms (24 MP) and 42 ms (60 MP) for Exposure's single multiply,
48 ms and 127 ms for Exposure plus the three composed Tone curve stages, 73 ms and 204 ms for the
fused Oklab `ColourAdjust`, and 21 ms and 58 ms for the one composite 3 × 3 linear-sRGB white-balance
matrix. The neutral picker is not a frame operation: it evaluates 25 point samples of the stage the
Basic layer receives at `O(layers)` each and allocates no frame, so its cost does not grow with the
pixel count.

**Superseded rows.** This table replaces the following earlier records, each of which came from its
own separate invocation on the same host and is no longer the current figure: the crop-module table
(one transform 10.7 / 11.2 ms and 22.7 / 26.0 ms, 200 composed transforms, and the 10° `crop-fit`
stack at 33.2 / 37.7 ms and 70.5 / 77.0 ms); the orientation-layer re-measurement of the 24 MP rows
(10.3 / 14.3, 10.5 / 11.1 and 31.5 / 34.0 ms, with the `crop-fit` commit falling from 1.2 to 0.6 ms
once it plans against a one-layer prefix); the Exposure row (56.3 / 123.4 ms and 129.4 / 221.3 ms,
whose tails followed the allocator rather than the pass); the Exposure-plus-Tone row (95.7 / 139.1 ms
and 235.0 / 264.7 ms); the separate-unit Vibrance/Saturation row (150.8 / 163.2 ms and
402.3 / 594.1 ms) and its fused replacement measured under heavy host load (107.6 / 112.4 ms and
357.8 / 440.3 ms); and the Temperature/Tint run (50.9 / 54.9 ms and 122.4 / 133.3 ms with its own
31.8 / 34.4 ms and 68.9 / 73.8 ms baseline). The conclusion those rows were recorded for still
holds — fusing Vibrance and Saturation into one Oklab round trip removed a whole conversion pair,
and `ColourAdjust` remains the most expensive single unit — but the numbers above are the ones to
quote.

### The masked colour primitive, its own run

On the build measured here, a masked colour layer cost the units it would have cost unmasked, plus one
coverage evaluation and one blend per pixel **inside the mask's bounds rectangle**, and nothing at all
outside it; inside the rectangle its units and blend now run only where the coverage is not zero
([below](#units-only-where-the-coverage-is-not-zero)). Measured on
the host above, release, single invocation, three measured renders after one warm pass, over a
programmatically filled 6000 × 4000 frame with one `+1 EV` exposure unit
(`render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame`, an ignored measurement test):

| 24 MP render, one colour unit | ms per render |
| --- | --- |
| Identity recipe (shared source buffer) | under 0.05 |
| Unmasked | 42.9 |
| Masked, gradient bounds admitting 5.05% of the frame | 34.1 |
| Masked, gradient bounds admitting 100% of the frame | 45.6 |

So the bounds rectangle is worth 11.5 ms of the 45.6 here, and a mask over the whole frame costs
about 2.7 ms — 6% — more than no mask at all. The rectangle does **not** remove the pass's decode and
quantization of the rows it touches, because the frame must still be written: skipping a whole row
chunk when every operation in its run is masked and the chunk lies outside every rectangle is possible
and is not built. The unit-evaluation claim itself is asserted rather than inferred, by a counting
colour unit in
`render::mask_tests::a_masked_operation_evaluates_no_unit_outside_its_bounds`, which requires the count to
equal the rectangle's pixels whose coverage is not zero, and the rectangle's area exactly under the
rule before, kept for the tests.

`editor-performance` on 24 MP, 30 samples, after the change: colour baseline 33.1 / 38.3 ms and one
`+1 EV` Basic layer 40.3 / 45.5 ms, both inside the recorded ranges above, on a host whose load was
shared with other sessions. There is no paired before-run from this worktree; the unmasked path's
arithmetic is unchanged by construction and proved byte-identical by the colour tests, and the mask is
consulted once per operation per row rather than per pixel.

#### Units only where the coverage is not zero

Inside its bounds rectangle a masked colour operation evaluates each pixel's coverage first and runs
its units only over the stretches of pixels whose coverage is not zero, or whose input holds a −0.0 or
a value that is not finite ([masking](../design/masking.md)); a pixel it skips keeps its input, the bit
the blend gives there, so every output is the same to the bit (`a_pixel_whose_coverage_is_zero_runs_no_unit_and_keeps_its_bits`,
`a_render_that_skips_uncovered_pixels_is_identical_and_samples_equal_it`).
`render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame` times one exposure unit and a full
Basic layer over a programmatically filled 6000 × 4000 frame, unmasked and under four masks, each
masked case under the rule before and the rule now back to back, and prints the process's CPU time
beside the wall time and the least of 30 single-thread passes over 64 rows. Release build, Apple M4
Pro, a shared host at a one-minute load of 21 to 62, which moved the wall times by up to four times:
the CPU times and the single-thread figures are the stable ones. Two alternated pairs:

| Case, 24 MP | Non-zero coverage | CPU ms per render, before | After | Single thread, ns per pixel, before | After |
| --- | ---: | ---: | ---: | ---: | ---: |
| Basic, bright band (80–100) | 14.95% | 2,025 / 2,046 | 883 / 894 | 67.4 / 68.6 | 24.8 / 24.8 |
| Basic, middle band (25–38) | 25.40% | 1,981 / 1,986 | 1,066 / 1,075 | 68.4 / 68.6 | 31.6 / 31.6 |
| Basic, whole-frame gradient | 100% | 1,648 / 1,667 | 1,672 / 1,678 | 54.5 / 54.6 | 53.8 / 53.9 |
| Basic, unmasked | – | 1,490 / 1,499 | 1,496 / 1,497 | – | – |
| Exposure, bright band | 14.95% | 626 / 657 | 675 / 681 | 19.6 / 19.7 | 18.2 / 18.3 |
| Exposure, whole frame | 100% | 266 / 275 | 290 / 294 | 5.44 / 5.47 | 4.80 / 4.81 |

- **A band mask over a full Basic layer** takes 46 to 57% less CPU than it did (the middle band 46%, the bright
  band 56 to 57%), and its wall time is 78 to 114 ms against 172 to 176. On the generated `24mp.jpg` (`LUXFORGE_MASKED_COLOUR_SOURCE`),
  whose four flat quadrants a band selects whole, one pair at a load of 21 to 24 took 877 and 890 CPU
  ms against 1,798 and 1,834.
- **A single exposure unit** costs too little for the units skipped to show: its render's CPU time
  moved within ±10% with no consistent direction across sessions, the band's cost being its
  coverage's luminance encode at every pixel of the stage.
- **Coverage that alternates every pixel**, one-pixel stretches, is the worst case: one exposure unit
  took 12.4 to 14.2 ns a pixel against 10.1 to 12.1, and a full Basic layer about the same either way.

```sh
cargo test --release --package luxforge-core --lib render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
LUXFORGE_MASKED_COLOUR_SOURCE=fixtures/generated/24mp.jpg cargo test --release --package luxforge-core --lib render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
```

### The masked spatial primitive, one to sixteen layers

A masked spatial layer costs what it would have cost unmasked, plus one coverage evaluation and one
blend per pixel of the tiles the mask's bounds rectangle reaches, minus the whole unit chain of every
tile it does not. Each spatial layer, masked or not, is a stage boundary and therefore a **sequential
full frame** in the reference renderer's frame and in its export. A recipe holds at most 16 masked Presence and
Detail layers between them (owner, 2026-10-03, [decisions](../decisions.md#gpu-previews)), and the
host refuses a seventeenth with a `resource-limit` error naming the limit.

`cargo test --release --locked --package luxforge-core --lib -- --ignored masked_spatial_timing
--nocapture` (`render::spatial::tests::masked_spatial_timing`), on in-memory synthetic frames
rendered by the core alone, warm source and warm estimate store, one `luxforge.presence` Clarity
`+100` layer per mask: the CPU's exact render, which an export runs before it encodes. "Whole frame"
is a gradient whose bounds rectangle is the entire stage; "right-edge band" is one confined to about
a tenth of the columns. Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2; the release build of local
main at `946751cc`, which holds the [efficiency](../design/efficiency.md) work beside the GPU
preview's scratch pool and the cap of 16, 2026-10-04. The one-minute load was 2.5 at the start and
rose to 12 during the run, because the render drives the whole Rayon pool; the figure before it
starts is the other sessions' load. p50 / p95 of 5 runs, milliseconds:

| Stage | Mask | 1 | 2 | 4 | 8 | 16 masked layers |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 6000 × 4000 | whole frame | 130 / 136 | 270 / 278 | 559 / 562 | 1,148 / 1,162 | 2,351 / 2,378 |
| 6000 × 4000 | right-edge band | 58 / 67 | 124 / 128 | 242 / 252 | 508 / 514 | 1,034 / 1,042 |
| 10000 × 6000 | whole frame | 378 / 398 | 839 / 859 | 1,664 / 1,708 | 3,419 / 3,583 | 6,745 / 6,850 |
| 10000 × 6000 | right-edge band | 153 / 172 | 312 / 318 | 647 / 681 | 1,338 / 1,350 | 2,687 / 2,732 |

An unmasked single layer takes 94 / 97 ms at 24 MP and 299 / 307 ms at 60 MP.

- **Cost grows with the layer count, linearly**: about 147 ms a whole-frame masked layer at 24 MP
  and 420 ms at 60 MP, so 16 such layers are 2.4 s and 6.7 s of exact render. It is the reference
  renderer's whole frame, so it is not what the hand feels: a gesture over these layers is drawn
  on the GPU ([painting](#painting-over-masked-spatial-layers)), and a render expected to take more
  than a second shows its progress on the photograph.
- **A tile outside the bounds rectangle costs no unit evaluation**, so a small mask is cheaper than
  the unmasked layer: a right-edge band's layer takes 58 ms at 24 MP and 153 ms at 60 MP, against
  94 and 299 unmasked. The band copies 80 of 96 tiles at 24 MP and 204 of 240 at 60 MP, exact
  counters read from the host (`masked_tile_counts`) on the build before the efficiency work.
- **A whole-frame mask** adds a coverage evaluation and a blend at every pixel: 130 against 94 ms at
  24 MP and 378 against 299 ms at 60 MP at one layer. A masked tile holds one extra tile-sized plane,
  the snapshot the blend is against, which on the build before the efficiency work put the spatial
  budget's peak at 255.3 MiB at 24 MP and 239.7 MiB at 60 MP and moved the 60 MP plan's concurrency
  from 8 tiles to 7.
- The blend is **in place** in the last unit's planes. An earlier spelling that copied the tile out
  and blended into a second buffer measured 4464 ms against 1873 at 60 MP — a 2.4× overhead from two
  fresh tile-sized allocations per tile, not from arithmetic. That spelling is not what shipped, and
  it is recorded because it is the trap: the blend is cheap and the allocations were not.

On the **RAW linear path** each spatial operation materializes one `f32` frame, and each frame
replaces the one before it, so at most two exist at once whatever the number of masked spatial
layers, as on the byte path.

### The masked spatial primitive, tiles the mask leaves uncovered inside its bounds

A masked Presence layer (Clarity +100) on an in-memory 6000 × 4000 frame whose mask reaches most of
its bounds rectangle but covers little of it: a diagonal gradient, an inverted radial (bounds are the
whole stage) and a luminance range (a value-based component, whose bounds are always the whole
stage). Each is rendered under the rule before tiles were proved uncovered one by one — copy only
outside `bounds()` — and under the proof, alternating run by run and swapping which goes first, with
every pair of frames asserted byte-identical. `cargo test --release --locked --package luxforge-core
--lib -- --ignored masked_spatial_zero_coverage_timing --nocapture`, M4 MacBook Pro, p50 and the
slowest of 6 runs each. The p50 recorded here is the 4th of 6, the test's upper median before it read
the shared nearest-rank `Distribution`, whose p50 is the 3rd.

| Mask | Outside bounds only: p50 / slowest ms | Proved per tile: p50 / slowest ms | Tiles copied / evaluated, before → after |
| --- | --- | --- | --- |
| Diagonal gradient toward the top-left corner | 300 / 1466 | 268 / 1154 | 16 / 80 → 37 / 59 |
| Inverted radial (a vignette) | 262 / 294 | 213 / 406 | 0 / 96 → 33 / 63 |
| Luminance range 70 to 100 | 297 / 396 | 223 / 242 | 0 / 96 → 45 / 51 |

**Provisional.** The one-minute load average was 9.9 before the run and 22.5 after it, well above
the 8.0 a quotable figure needs, so the milliseconds are an upper bound and the slowest column is
noise. The tile counts are exact counters (`masked_tile_counts`) and are the result: a third to a
half of the tiles skip the unit chain, and the p50 falls by 11 to 25% in the same interleaved run.
The saving in wall time is smaller than the share of tiles because a render also pays for the global
estimate and the fill, and the tiles that remain still run in parallel.

### The masked spatial primitive, a mask of many components

The other half of the same cost: one masked Presence layer whose mask holds 1, 4, 16 and 32
components — [the limit](../design/masking.md) — each a linear gradient across the whole frame, so
the bounds rectangle is the whole stage and **every** component is evaluated at every pixel. The
modes cycle through add, subtract and intersect, because those are one `max` and two `min`s per
pixel and nothing else. `cargo test --release --locked --package luxforge-core --lib -- --ignored
masked_spatial_component_timing --nocapture`, same host, same warm-up, p50 and the slowest of 5
runs, one-minute load average 5.12 at the start.

| Stage | Components | p50 / slowest ms | Tiles copied / evaluated | Budget peak |
| --- | --- | --- | --- | --- |
| 6000 × 4000 | 1 | 216 / 226 | 0 / 96 | 255.3 MiB |
| 6000 × 4000 | 4 | 239 / 305 | 0 / 96 | 255.3 MiB |
| 6000 × 4000 | 16 | 306 / 311 | 0 / 96 | 255.3 MiB |
| 6000 × 4000 | 32 | 380 / 384 | 0 / 96 | 255.3 MiB |
| 10000 × 6000 | 1 | 844 / 844 | 0 / 240 | 239.7 MiB |
| 10000 × 6000 | 4 | 877 / 891 | 0 / 240 | 239.7 MiB |
| 10000 × 6000 | 16 | 1042 / 1060 | 0 / 240 | 239.7 MiB |
| 10000 × 6000 | 32 | 1204 / 1243 | 0 / 240 | 239.7 MiB |

**A component is cheap and the cost is linear in the count.** Thirty-one further components add
164 ms at 24 MP and 360 ms at 60 MP — **5.3 ms and 11.6 ms each**, a ratio of 2.2 against the 2.5
the pixel counts predict, which is what a per-pixel field evaluation looks like. A mask at the
32-component limit costs 76% more than a one-component mask at 24 MP and 43% more at 60 MP, and the
whole 32-component evaluation is still smaller than the Presence chain it modulates. The limit of 32
is not a performance limit at these sizes; it is a limit on how much a person can keep track of.
This is the worst case by construction: a real mask's components have bounded supports, and a
component whose support the tile does not touch is not evaluated there at all.

### The brush's own workload

What a painted mask costs, as against the gradients whose cost is already recorded above: its compile
(the grid index over segments, built before a pixel is read), the rectangle it bounds, the render it
modulates, and the point query it answers. `cargo test --release --locked --package luxforge-core
--lib -- --ignored masked_brush_cost_on_photo_sized_frames --nocapture`
(`render::mask_tests::masked_brush_cost_on_photo_sized_frames`, an ignored measurement test), on the M4
MacBook Pro, release, one warm-up render then the mean of three, twenty compiles, and a thousand
point queries spread over the frame. **One-minute load average 3.96 before the run and 9.87 after**,
the run itself taking 8.8 s; the figures below are therefore taken on a quiet host by the
[reliability rule](#provisional-targets-measured), and the rise is this measurement's own tail.

Every stroke is the panel's own brush — radius 0.05 mask-space units, which is 200 px on a 24 MP
stage, feather 50, flow 100 — laid as a three-position path across its own band of the frame, so the
strokes neither coincide nor leave it. The masked layer is one Exposure unit, the same one the
masked-colour row above uses, so the difference between the rows is the mask and nothing else.

| Mask on one Exposure layer | 24 MP ms | 60 MP ms | Rectangle |
| --- | --- | --- | --- |
| None (unmasked exposure) | 35.2 | 83.4 | — |
| Whole-frame linear gradient | 66.6 | 174.6 | 100% |
| Brush, 1 stroke | 41.5 | 105.3 | 10.5% |
| Brush, 8 strokes | 103.2 | 250.4 | 71.2% |
| Brush, 32 strokes | 152.3 | 384.8 | 77.7% |
| Brush, 64 strokes — [the limit](../design/masking.md) | 213.9 | 537.0 | 78.8% |

- **A brush is a gradient with a smaller rectangle.** One stroke costs 41.5 ms against the
  whole-frame gradient's 66.6 at 24 MP, because its conservative rectangle admits a tenth of the
  frame and the pass skips the rest — the same saving the masked colour primitive's own rows record.
  A mask is never free: one stroke is 6.3 ms over no mask at all at 24 MP and 21.9 at 60 MP.
- **Each further stroke costs about 2 ms per 24 MP frame and 5 ms per 60 MP frame**, once the
  rectangle has stopped growing: 8 → 32 strokes is 2.0 and 5.6 ms a stroke, 32 → 64 is 1.9 and
  4.8 ms, a ratio of 2.5 against the 2.5 the pixel counts predict. That is what a per-pixel field
  evaluation looks like, and it is the same shape the component table above measures.
- **The compile is not a cost worth naming.** Building the grid index takes 0.003 ms for one
  stroke and 0.029 ms for sixty-four at 24 MP, and less at 60 MP because the work is in the strokes
  rather than the stage. It is charged to the gesture, once, before a pixel is
  read — a thousandth of the frame it precedes.
- **A point query stays a point query.** `Render::sample` through a brush mask answers in 0.0023 ms
  at one stroke and 0.0385 ms at sixty-four, on both stage sizes: it rasterizes nothing
  ([rule 4](../engineering/performance-rules.md#rules)), and the cost it does have is the segments
  the index leaves near that pixel. Even at the stroke limit it is a four-hundredth of a display
  frame, so the eyedropper and `render.sample` are unaffected by how much has been painted.

**Scope.** These are the reference renderer's whole-frame renders on the calling thread, which on
that build the histogram, the overlays and the 100% view took; what a hand felt during a stroke then
was the proxy phase, whose masked figures are in [Masks in the proxy phase](#masks-in-the-proxy-phase)
below, and a stroke is now drawn on the GPU ([painting](#painting-over-masked-spatial-layers)). The 64-stroke
row is a worst case by construction — sixty-four full-width strokes is far past the point where a
second component is the better answer — and it is the delivered per-component limit rather than a
recommendation.

### Masks in the proxy phase

The proxy measured here is the CPU proxy, which since the GPU-first stage 5 draws a drag only in a
session without a GPU ([instant previews](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu));
the desktop drags below were drawn by it on their builds. A masked recipe is proxy eligible by construction: mask geometry is stored normalized, so the mask
compiled against the proxy stage is the same field at a smaller scale and the proxy frame is the
exact recipe at proxy size. The only thing that changes with the stage is sampling, and the recorded
default ([masking](../design/masking.md#point-queries-and-proxies), proposal P5) supersamples the
**mask field only**, 2 × 2 per pixel, when the mask's narrowest feature is under two pixels of the
proxy stage.

Core cost of the proxy render a drag presents, measured on the host above, release, 25 measured
renders after one warm pass against a cached proxy source, display bounds 2880 × 1800
(`cpu_proxy::tests::measure_the_masked_proxy_render_on_photo_sized_sources`, an ignored measurement
test). One-minute load average 6.8 at the start and 7.1 at the end, so these are **provisional**
figures on a host shared with other sessions, not a quiesced baseline.

| Proxy render, full Basic layer | 24 MP → 2700 × 1800 | 60 MP → 2880 × 1728 |
| --- | --- | --- |
| Unmasked | 26.6 / 28.9 ms | 29.3 / 32.8 ms |
| Masked, broad gradient, point sampled | 23.7 / 27.0 ms | 25.7 / 29.2 ms |
| Masked, thin gradient, point sampled | 17.7 / 20.0 ms | 19.0 / 23.6 ms |
| Masked, thin gradient, 2 × 2 supersampled | 19.8 / 21.2 ms | 20.4 / 22.3 ms |

p50 / p95. The last two rows are the **same stack at the same size**, so their difference is the
thin-feature rule alone: **+2.1 ms p50 at 24 MP and +1.4 ms at 60 MP**, 7–12%, for four coverage
evaluations per covered pixel instead of one. A mask never costs more than no mask here, because its
bounds rectangle skips the spans it cannot reach — the same saving the masked colour primitive's own
run records — so the rule's cost is paid only inside the selection.

Desktop input-to-presented-frame for a slider drag at Fit, full Basic layer, `editor-latency --mode
drag --basic`, 30 samples, two runs at each size:

| Drained drag at Fit | 24 MP | 60 MP |
| --- | --- | --- |
| p50 | 17.8 / 17.6 ms | 21.5 / 17.6 ms |
| p95 | 48.2 / 123.5 ms | 26.2 / 26.0 ms |
| min | 16.1 / 16.4 ms | 15.8 / 15.5 ms |

Against the provisional target of p95 below 16 ms with 32 ms acceptable: **the p50 is 17.6–17.8 ms on
both sources and the minimum is 15.5–16.4 ms, so the target is missed at the median by under two
milliseconds; the p95 is not a usable figure from this host.** The one-minute load average was 11.1
and 8.8 for the first pair and 5.8 and 6.5 for the second, all at or above the 8.0 the reliability
rule in [provisional targets](#provisional-targets-measured) sets for quoting a baseline, and the
24 MP p95 of 123.5 ms comes from a single outlier against a maximum of 123.9 ms and a p50 of 17.6 ms.
These figures are recorded as provisional and are a finding for the owner, not a verdict. `draft.set`
round trip was 0.21 ms p50 in every run, unchanged, which is the hop rule holding.

#### An end-to-end masked slider drag

This is now drivable and was taken. `editor-latency --mask` draws a linear gradient through
`mask.create-linear`, enters Mask mode and opens the mask **by the name the host gave it** — the
evidence script resolves a `{"name": …}` reference against the `mask.list` answer the desktop holds,
which is what removed the blocker recorded here before: `mask.create` assigns the identity, so a
script that creates a mask has nothing else to name it by. From the selection on, every generated
slider gesture carries that mask, exactly as the panel's own drag does. With `--basic` beside it the
measured stack holds a global full-Basic layer *and* a masked Basic layer, so each frame runs the
module's whole colour pass twice, once of it through the masked colour primitive.

**These figures replace the first, contended ones, which were taken at load 39.5 to 57.5 and are
withdrawn.** Eight runs, 30 samples each, drag mode, at Fit, taken back to back and then reversed —
24 MP unmasked, 24 MP masked, 60 MP unmasked, 60 MP masked, then the same four in the opposite
order — each preceded by a 100-second pause, so the one-minute load average read beside it is the
*other sessions'* load and not this measurement's own tail. Those loads are in the last row. The
two figures in each cell are the forward run and the reversed one.

| Drained drag at Fit, `--basic` | 24 MP unmasked | 24 MP masked | 60 MP unmasked | 60 MP masked |
| --- | --- | --- | --- | --- |
| p50 | — / 16.93 ms | 16.77 / 17.13 ms | 17.00 / 17.48 ms | 17.06 / 17.87 ms |
| p95 | — / 91.7 ms | 25.3 / 25.7 ms | 25.4 / 26.8 ms | 25.1 / 26.2 ms |
| min | — / 15.12 ms | 15.45 / 15.74 ms | 15.25 / 15.46 ms | 15.70 / 15.44 ms |
| `draft.set` round trip p50 | — / 0.226 ms | 0.227 / 0.231 ms | 0.214 / 0.215 ms | 0.231 / 0.236 ms |
| one-minute load before the run | — / 4.19 | 8.13 / 3.37 | 3.65 / 3.32 | 2.34 / 2.58 |

**The first run of the sequence is discarded and is shown as `—`.** It measured p50 41.40 ms, p95
135.7, min 25.95 and a `draft.set` of 0.395 ms — every one of them roughly double every other run's,
including the hop, which no amount of render load moves. It was the first launch after the machine
had been idle, so it paid for cold shader, filesystem and allocator state; the identical run at the
other end of the sequence read 16.93 ms. That is a finding about the harness and not about masking,
and it is recorded rather than averaged away: **the first `editor-latency` launch after an idle
period is not a usable sample.**

**A mask costs a person's hand nothing measurable.** Every remaining p50 is between 16.8 and
17.9 ms, masked and unmasked, at both sizes. The largest difference between a masked run and its
unmasked pair is 0.4 ms, which is smaller than the difference between the two runs of the same
condition. A second set of eight runs taken back to back with no pause between them — and therefore
at load 12.1 to 17.4, this measurement's own tail — agrees: 17.64 and 16.75 ms 24 MP unmasked,
16.63 and 16.52 masked, 16.23 and 16.12 ms 60 MP unmasked, 15.76 and 15.91 masked.

- `draft.set` round trip p50 is **0.214–0.226 ms unmasked and 0.227–0.236 ms masked** — a mask
  target adds about 0.01–0.02 ms to the gesture's own hop, on every run, in both orders. The hop
  rule holds through the masked path.
- The queue counters are identical in all sixteen runs: 31 scripted values in the burst step
  coalesced into **1** `draft.set`, 32 `draft.set` requests, 32 preview jobs requested and **0**
  superseded, two commits. A masked drag coalesces and drains exactly as an unmasked one does;
  nothing about the mask adds a round trip, a job or a dropped frame.
- Every run passed its own scripted-step checks, so each measured input is the scripted value with
  its own `draft.set`, preview job and displayed frame.

Against the provisional target (p95 below 16 ms, acceptable below 32 ms): **every run is inside the
acceptable bound at the p95 and every one misses the 16 ms target, and masking does not change
which.** The one exception is the 24 MP unmasked p95 of 91.7 ms, a single stray sample against that
run's own minimum of 15.1 ms and p50 of 16.9. The median misses the target by about a millisecond
everywhere. That is the same miss the unmasked rows above already record.

#### A masked Presence slider

The figure this document previously recorded as outstanding. `editor-latency --action set-presence
--parameter clarity`, with and without `--mask`, 24 MP, 30 samples, drag mode, at Fit, taken in both
orders with the same 100-second pause before each. Clarity is a spatial operation and therefore a
stage boundary, so this is the hand's-eye view of the row the core table above measures at exact
size.

| Drained Clarity drag at Fit, 24 MP | unmasked | masked |
| --- | --- | --- |
| p50 | 17.00 / 17.10 ms | 17.18 / 17.10 ms |
| p95 | 18.2 / 25.4 ms | 18.6 / 24.0 ms |
| min | 14.64 / 14.86 ms | 14.04 / 15.27 ms |
| `draft.set` round trip p50 | 0.202 / 0.209 ms | 0.226 / 0.237 ms |
| one-minute load before the run | 3.67 / 1.85 | 3.96 / 1.99 |

**A masked Presence drag is indistinguishable from an unmasked one at the median**: 17.10–17.18 ms
against 17.00–17.10, a difference of at most 0.18 ms against a run-to-run spread of 0.10 ms in
either condition. The hop pays the same 0.02–0.03 ms a masked Basic drag does. The queue counters
are again identical: 31 values into 1 `draft.set`, 0 superseded. Masked Presence therefore meets the
same provisional bound the unmasked spatial slider does — acceptable at the p95, missing the 16 ms
target at the median by about a millisecond. This was the proxy phase, what the hand felt on that
build; the exact phase behind it is the core table above.

`editor-performance --samples 30` was re-run on the same host beside the first, contended drags,
release, warm cache, on `24mp.jpg` (load average 8.6 rising to 17.3) and `60mp.jpg` (load average
17.3 rising to 20.8). Both **passed every check**, including that the catalog reopen reconstructs the
original historical state and the source SHA-256 is unchanged, so the phase leaves the core's own
correctness diagnostics intact. Their timings are not quoted, for the reason the drags were not: the
harness's own full-Basic core render read 128.2 ms p50 / 224.5 ms p95 at 24 MP and 527.1 / 619.1 ms
at 60 MP, and a p95 more than 1.7 times its own p50 in a warm 30-sample loop is a measure of the
host's queue, not of the render.

#### A painted stroke's own latency

These worker timings describe the reference preview path in this measurement. Current GPU painting and its presentation tails are measured separately in the GPU-first sections.

The `editor-latency --mode paint` workload measures a brush stroke at a fixed 24 ms input pace.
The harness pairs each `mask_draft_set` with the preview generation returned for that edit and then
with the corresponding `preview_displayed` event. The editor emits `mask_draft_preview` for that
pairing. When diagnostics are enabled for the paint measurement, the queue and worker record request
to worker-start and worker-render durations; ordinary preview requests remain untimed. The scripted
stroke step accepts `interval_ms` and sends one point per timer tick, so the sample is a continuous
stroke with one history entry, not a sequence of released strokes.

The four-layer correctness scenario and the bare-recipe photo-sized workload use different recipes
and should not be combined into one estimate. The bare-recipe phase table below isolates the
per-generation owner, queue, worker and surface intervals.

#### The four-layer figure, from the correctness run

The `mask-range` scenario's own, recorded in its `result.json` beside the recipe they were taken on and
the load the host was under. Twelve positions at a 24 ms interval — a little over the delivered
masked-drag median of 16.8–17.9 ms, so each position has a round trip of its own to finish — painted
down the middle of one flat patch of a 1440 × 960 fixture. Two runs, back to back, on a quiet host.

| Painted stroke, `mask_draft_set` → `preview_displayed` | run 1 | run 2 |
| --- | --- | --- |
| p50 | 32.6 ms | 32.2 ms |
| p95 | 50.2 ms | 42.1 ms |
| drafted frames displayed / inputs | 11 / 17 | 10 / 17 |
| one-minute load average | 1.58 | 1.80 |

**Against the provisional target (p95 under 16 ms, acceptable under 32 ms) this is a miss at both
bounds, stated plainly: the p50 alone is already at or past the acceptable p95.** The load average is
well under the 8.0 a quotable figure needs, so it is not the host.

That recipe is also the heaviest the scenario builds: **four masked colour layers**, three of whose masks
hold a luminance range or a colour range. A value-based component answers the *whole stage* for its
conservative rectangle, by [P13](../design/range-study.md#proposals), so on that build those three
layers were evaluated at every pixel with no span skipped — the cost the range study measured at 28–44 ns per pixel
over 100% of the stage, against a placed gradient's 14.6–20.0 ns over 40%. It was recorded here with
the reading that most of the figure was that recipe. **The bare-recipe measurement below withdraws
that reading**, and the correction is the more useful of the two results.

#### Bare recipe at photo size, with phase attribution

`editor-latency --mode paint` opens one generated photo-sized JPEG, prepares a single brush mask and
one masked Basic exposure layer, then sends a stroke at 24 ms intervals. The rows in this section
were taken with the workload's earlier stroke: 30 positions along a straight sweep with a hard-edged
brush (feather 0), which took the mask field's 2 × 2 proxy supersample on every frame and decimates
to its two ends; the current feathered, curved stroke is measured
[below](#mask-feedback-and-the-coverage-handoff). The parser
starts at that scripted stroke step, so setup edits do not enter its latency or superseded counts.
The preview queue remains one active job plus one replaceable pending job; every frame is paired to
the generation its own `mask_draft_preview` named. Release build, warm file cache, background hidden
window on the Apple M4 Pro, 2880 × 1800 physical window at 2× scale. All presented phases were the
1716 × 1144 display proxy. One run per size, no competing build or test; start/end one-minute load
was 3.91/3.91 for 24 MP and 3.46/3.46 for 60 MP.

| Phase (p50 / p95 ms) | 24 MP, 6000 × 4000 | 60 MP, 10000 × 6000 |
| --- | ---: | ---: |
| `mask_draft_set` → `preview_displayed` | 19.23 / 40.70 | 17.71 / 35.06 |
| Owner round trip to preview request | 0.18 / 5.96 | 0.12 / 0.65 |
| `draft.set` on owner | 0.04 / 3.23 | 0.03 / 0.16 |
| Preview-job planning on owner | 0.13 / 4.34 | 0.09 / 0.19 |
| Preview request → worker start | 6.63 / 18.17 | 8.59 / 17.40 |
| Worker render | 4.63 / 5.43 | 4.14 / 4.45 |
| Pre-result residual | 7.29 / 20.28 | 4.97 / 22.41 |
| Worker result → surface assignment | 0.02 / 0.04 | 0.01 / 0.02 |

The owner executor wait and return-to-queue legs stayed below 0.01 ms in these samples. The
pre-result residual is the remaining time from the input event through the request timestamp and
from worker completion until the app polls the result; it does not identify one function. Phase
percentiles are independent distributions and must not be summed. The request-to-worker interval
includes time behind an active job and thread scheduling. GPU texture upload and display scanout
were not measured; the last row is only the app's surface-assignment event.

| Workload resources | 24 MP | 60 MP |
| --- | ---: | ---: |
| Displayed positions / 30 | 26 | 24 |
| Superseded positions | 4 | 6 |
| Process CPU seconds for the whole evidence launch | 7.34 | 8.73 |
| Sampled peak process RSS | 785.4 MiB | 1288.2 MiB |
| Shared colour scratch high-water mark | 15.12 MB | 15.12 MB |

The CPU total includes source opening, initial rendering, the gesture, evidence captures and process
exit; it is not a per-frame CPU measurement. Peak RSS includes captures and GPU resources and is not
a CPU-heap figure. The 60 MP peak from this capture-heavy run is not comparable to the normal-open
working-set target above. The synthetic fixtures isolate image dimensions and fixed per-pixel work;
they are not camera-photo content.

**The measurable tail is scheduling and delivery rather than the pixel kernel.** Worker render p95
is 4.45–5.43 ms, and surface assignment is at most 0.04 ms, while input-to-surface p95 remains
35.06–40.70 ms. Queue wait and the pre-result residual dominate the tails; some owner `draft.set`
round trips also have a several-millisecond tail on 24 MP. This does not justify SIMD, assembly, a
GPU rewrite or changing the mask renderer yet. Keep the bounded queue and cancellation rules while
tracing whether the remaining gap comes from the active-job handoff or the desktop event loop.

#### Zeroed frames and an identity pass's source rows

A byte frame is an `Arc<Vec<u8>>` allocated with `vec![0; len]`, which the system allocator serves with `calloc`, so no thread fills it before the pass that writes it; an identity pass over the shared source loads the source's rows inside its parallel pass (one `copy_from_slice` per row) instead of copying the whole source on one thread first. `editor-performance --samples 30` on the generated JPEGs, release `--locked`, the base `2360cf82` with the Exposure-only row added (before) against this change (after), in-process on the native Apple M4 Pro, 28 September 2026, holding the host-wide timing lock. Two rounds per size, before, after, after, before and then after, before, before, after, at a one-minute load of 13.5 to 19.6 (24 MP) and 19.1 to 28.5 (60 MP) from other sessions' builds. Each cell is the p50 of each run in the order the runs went, first round then second; the p95s followed the load and are in the evidence, not here. Exposure-only is one +1 EV Basic layer on the upright source and nothing else; one exact transform is a quarter turn.

| Render (p50 ms) | Before | After |
| --- | --- | --- |
| 24 MP Exposure-only | 8.00 · 17.38 · 16.92 · 11.48 | 7.59 · 9.24 · 6.66 · 12.81 |
| 24 MP one exact transform | 6.33 · 15.44 · 13.65 · 8.58 | 6.27 · 15.00 · 6.31 · 6.70 |
| 60 MP Exposure-only | 36.27 · 20.23 · 23.47 · 25.72 | 19.89 · 19.65 · 25.25 · 24.45 |
| 60 MP one exact transform | 14.19 · 12.96 · 15.47 · 15.31 | 14.32 · 16.32 · 13.32 · 25.64 |

Every run's frames had the same SHA-256 before and after: `cf45865f…` (24 MP Exposure-only), `4895b6de…` (24 MP one transform), `0c36dca4…` (60 MP Exposure-only) and `21cb00ac…` (60 MP one transform). Only 24 MP Exposure-only moves: it is faster in three of the four adjacent before/after pairs (by 0.4 to 10 ms at the median, the larger gaps while the load climbed) and 1.3 ms slower in the fourth, and its fastest run falls from 8.0 to 6.7 ms. The one-transform renders and 60 MP Exposure-only lie within the runs' own spread at this load: the fastest runs are 6.33 against 6.27 ms, 12.96 against 13.32 ms and 20.23 against 19.65 ms.

The allocation itself explains why the difference is small in a warm loop. A release probe of a 240 MB frame (60 MP RGBA), 14 writer threads: in a fresh process, collecting `repeat_n(0, len)` into an `Arc<[u8]>` is a `malloc` and a serial `bzero` of 10.8 to 12.5 ms, and 12.2 to 13.7 ms with the parallel write after it; `Arc::new(vec![0; len])` returns in 2 to 3 µs, and the parallel write that faults its pages in takes the whole to 8.4 to 9.8 ms (10 processes each). At 96 MB (24 MP) the fresh-process totals are 4.7 to 5.5 ms for the fill and 6.3 to 6.9 ms for the zeroed allocation, so faulting pages in from every writer at once does not pay at that size. Repeating the same allocation in one process, as `editor-performance` does, the allocator hands back the region the last frame freed and both forms cost 1.0 ms to allocate and 2.0 ms with the write (p50 of 30). The render therefore gains most on a first render at a new size and on the identity pass's source copy it no longer makes.

#### Mask feedback and the coverage handoff

Native Apple M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, Metal, release `--locked`,
2 October 2026. Hidden background bundles, 2880 × 1800 physical pixels at 2×. The generated
24 MP JPEG is at Fit; the 60 MP JPEG is at 100%. Each launch paints 240 positions at 24 ms on
the harness's sine path, radius 0.06, feather 50, flow 100, with the selected mask's tint on.
The path is decimated by the current frozen rule, and each stroke commits one history entry.

Frozen baseline binary SHA-256 `72bdcfb472e5bfb1c277a362ba4f4dd935efa3dc818b051115ecd71eddffddb4`,
final optimized binary `e6db8e45d17e206ba3dc1b08349dc324683f61d1c2dff6881fb778e1ac63ec11`,
Cargo.lock `14235ffe8d1b76708d1f523fc5c171c4d04a42e78f237d32c2f6415cebfcaad1`.
Each size runs before, after, after, before, serialized under the timing lock with no local build
or test overlapping. A launch waits for one-minute load below 8: 7.53, 7.09, 6.75, 6.45 at
24 MP and 6.63, 6.76, 6.62, 7.14 at 60 MP. The first 60 MP baseline ends at 9.87 and
second optimized leg at 14.18; all other legs end below 8. The second 24 MP baseline has a
large worker slowdown despite load remaining below 8: one-minute load cannot guarantee a quiet
stroke. Every run and tail is retained.

The following cells retain both runs per variant, in their own execution order. Raw inputs,
accepted draft revisions, all distributions and resource samples live in
`artifacts/mask-perf-20261002/wake-abba-*`, summarized by `comparison.json`; the earlier
brush diagnostics are also retained there, with their binary identities and limits.

| Metric | 24 MP before | 24 MP after | 60 MP before | 60 MP after |
| --- | --- | --- | --- | --- |
| Photo adoption count | 239 · 216 | 239 · 239 | 232 · 239 | 239 · 234 |
| Authoritative coverage count | 237 · 190 | 239 · 239 | 213 · 237 | 239 · 205 |
| Photo adoption p50, ms | 8.20 · 13.12 | 8.30 · 8.30 | 8.96 · 7.88 | 7.75 · 8.92 |
| Photo adoption p95, ms | 16.28 · 62.12 | 16.23 · 15.75 | 45.79 · 16.23 | 15.12 · 36.12 |
| Authoritative coverage p50, ms | 8.22 · 13.25 | 8.33 · 8.50 | 8.69 · 7.90 | 7.84 · 8.94 |
| Photo event to overlay adoption p50, ms | 2.199 · 2.203 | 0.020 · 0.020 | 3.345 · 3.348 | 0.018 · 0.021 |
| Photo event to overlay adoption p95, ms | 2.275 · 12.625 | 0.025 · 0.025 | 10.102 · 3.590 | 0.024 · 15.634 |
| Preview worker p50, ms | 5.21 · 9.29 | 4.42 · 4.46 | 2.15 · 2.41 | 1.33 · 2.31 |
| Last-quarter preview worker p50, ms | 6.01 · 23.35 | 5.27 · 5.51 | 2.09 · 2.33 | 1.28 · 1.23 |
| Sampled peak process RSS, MiB | 700.8 · 708.8 | 649.8 · 661.3 | 1418.5 · 1394.2 | 1181.6 · 1133.6 |

The clear improvement is removal of UI-thread coverage painting: the worker returns shared painted
RGBA, reducing median overlay handoff from 2.2–3.35 ms to 18–21 µs, roughly 99%. The exact palette,
support rectangle and contiguous brush index preserve coverage while reducing work. Both optimized
24 MP legs deliver all 239 post-press positions; their preview-worker medians are 14–15% below the
first baseline, and their last-quarter medians 8–12% below it. The second baseline's slowdown is
reported, not used to inflate that saving. At 60 MP sampled RSS is 213–285 MiB lower across these
runs; this is a scoped process observation, including GPU resources and allocator retention,
rather than a CPU-heap or whole-editor memory guarantee.

A general end-to-end or tail-latency improvement is **not established**. The clean 24 MP photo
median stays around 8.3 ms, and one optimized p95 narrowly exceeds the provisional 16 ms target.
The clean 60 MP optimized leg is 7.75/15.12 ms p50/p95, but its loaded partner reaches
8.92/36.12 ms and delivers fewer positions. Pre-result scheduling and delayed coverage remain
visible in the raw distributions. Coverage wakes before a matching photograph are suppressed;
publishing photo content before draining coverage prevents lost wakes, while unchanged-photo
feedback and unavailable outcomes stay immediate. The tests prove that liveness; the timings do
not isolate this policy's saving. These are adoption timestamps, not GPU completion or display
scanout. The synthetic brush workload does not qualify RAW, high-density recipes or general mask
latency.

The [mask performance design](../design/mask-performance.md) records the unchanged numerical and
memory contracts. Exact brush/range/reference, cancellation, draft, source-preservation, bounded
handoff and wake/publication checks pass; final native `mask-interactions`, `mask-brush`, `mask-range`,
`viewport-region`, `performance` and `capabilities` frames correlate state and logs. Twelve native
Metal surface tests pass, with two separate diagnostic timing probes left out. Final quick
verification passes across 26 test binaries; 15 slow tests and doctests remain outside that tier.

#### Per-pass parallel thresholds

Each rendering pass kind runs on the shared Rayon pool from its own threshold (`render::limits::parallel_pixels`, [performance rule 9](../engineering/performance-rules.md#rules)), chosen from `render::parallel`'s `parallel_break_even_per_pass`: one unit per case through the built-in modules, serial against pooled (forced on the rendering thread) at 0.025 to 2 MP of the pixels that pass's gate counts, the two ways alternating sample by sample. Release test build on the native Apple M4 Pro, 28 September 2026, not holding the timing lock, while other sessions built: four runs, 21 samples per way (one-minute load 20.6 → 21.2), 31 (19.5 → 25.2), 41 for the spatial cases only (22.3 → 13.9) and 31 (7.5 → 44.4, a build starting part-way). Under that load a pooled run is the one that suffers, because a pool worker the scheduler has parked holds the join: pooled medians wander by several times between runs at every size, including the one-megapixel threshold every pass shared before, while serial medians stay within a few percent. The table therefore gives the pooled/serial p50 of the second run, which covers every case, and for the spatial cases the p50 and, after the slash, the ratio of the fastest runs from the third.

| Case (pooled/serial) | 0.025 MP | 0.05 MP | 0.1 MP | 0.25 MP | 0.5 MP | 1 MP | 2 MP |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Transform: quarter turn | 6.11 | 4.14 | 2.71 | 1.24 | 0.73 | 0.46 | 0.31 |
| Colour: Exposure, bytes | 1.55 | 0.89 | 0.58 | 0.31 | 0.26 | 0.20 | 0.16 |
| Colour: Exposure, linear | 0.78 | 0.51 | 0.34 | 0.19 | 0.18 | 0.14 | 0.13 |
| Heavy colour: full Basic, bytes | 0.31 | 0.35 | 0.44 | 0.19 | 0.13 | 0.12 | 0.12 |
| Heavy colour: full Basic, linear | 0.77 | 0.56 | 0.29 | 0.26 | 0.13 | 0.13 | 0.16 |
| Resample: 10° crop (output 0.034 … 2.78 MP) | 1.14 | 0.75 | 0.59 | 0.33 | 0.27 | 0.20 | 0.13 |
| Spatial: Texture +100 | 6.12 / 1.42 | 4.19 / 0.96 | 0.63 / 0.62 | 0.35 / 0.32 | 1.53 / 0.24 | 0.23 / 0.18 | 0.20 / 0.17 |
| Spatial: Clarity +100 | 1.40 / 1.21 | 0.85 / 0.78 | 0.53 / 0.47 | 0.28 / 0.25 | 0.56 / 0.19 | 0.41 / 0.16 | 0.32 / 0.15 |
| Spatial: Dehaze +100 | 6.62 / 2.12 | 4.60 / 1.87 | 7.22 / 0.98 | 1.73 / 0.49 | 2.29 / 0.44 | 1.56 / 0.54 | 0.25 / 0.21 |
| Spatial: all three +100 | 1.67 / 1.51 | 1.08 / 0.97 | 0.69 / 0.65 | 0.33 / 0.30 | 0.23 / 0.21 | 0.45 / 0.18 | 0.19 / 0.17 |
| Spatial: Clarity +100, linear | 0.97 / 0.85 | 0.61 / 0.55 | 0.37 / 0.33 | 0.49 / 0.21 | 0.18 / 0.17 | 0.16 / 0.15 | 0.16 / 0.14 |
| Proxy: box downscale to a third, JPEG | 0.88 | 1.00 | 1.00 | 1.23 | 0.69 | 0.54 | 0.52 |
| Proxy: box downscale to a third, RAW | 1.00 | 1.00 | 1.00 | 1.00 | 0.56 | 0.82 | 0.65 |

Each threshold is the smallest size at which every case of its kind won in every run: the transform 0.5 MP (1.24 to 1.26 at 0.25 MP in all three runs that measured it); one or two colour units 0.1 MP (bytes 0.89 to 0.93 at 0.05 MP); three or more units or a mask 25,000 pixels, the smallest size measured (a mask stays in this group, as before, and was not measured on its own); the resample 0.1 MP of output (1.14 at 0.03 MP; its fastest runs 0.46 at 0.07 MP); a spatial operation 0.25 MP of stage (at 0.1 MP Texture's fastest runs lost in the fourth run, 1.67, and Dehaze's broke even, 0.97 to 0.98); and the proxy 0.5 MP of source read, where up to 0.25 MP the downscale is one band and pooling changes nothing. A serial run at 0.025 MP often reports several hundred percent of one core: that is the pool still spinning down from the pooled run before it, which the CPU sampler charges to the process. RAW development, a spatial operation's global-estimate reduction, the analysis reducer and the overlays keep the one-megapixel threshold and were not measured here. Serial and pooled write the same bytes at a size between the old and the new threshold for every kind on both pixel domains (`slow_a_pass_between_the_shared_and_its_own_threshold_pools_to_the_serial_bytes`).

The Lens/Perspective `Warp` pass currently uses `PARALLEL_WARP_PIXELS = 100,000` output pixels, provisionally equal to the resample threshold. Its kernel has no measured break-even yet. `LUXFORGE_BREAK_EVEN_CASE=warp` selects the six serial/pool cases (Lens, Perspective and the fused Lens/Perspective/straightened crop, each in byte and linear domains) for the 0.025–2 MP study. Native Lens timing, memory and GPU residency remain incomplete; no existing resample ratio is evidence for this new pass.

On the RAW linear path the last segment's pass pulls a resample's taps through the segment before it (`load_resampled`), so its gate asks the resample's own threshold over the output, as the byte path's `resample_frame` does, and counts the pulled segment's colour units and masks with the segment's own: before, such a pass pooled only from the transform's 0.5 MP, and a straightened crop of 0.1 to 0.5 MP rendered serially. A byte segment pass after a warp, which resamples nothing itself, asks the transform's threshold for its geometry. `parallel_break_even_per_pass` gains the case `resample: 10 degree crop, linear`: at a one-minute load of about 5 on 3 October 2026 its pooled/serial p50 was 0.26 at 0.034 MP, 0.20 at 0.069, 0.14 at 0.139, 0.12 at 0.347, 0.11 at 0.694 and 0.10 from 1.04 MP, the byte crop in the same run 0.42 at 0.034 MP down to 0.11 at 2.78 MP, so pooling wins at every size and the 0.1 MP threshold stands. A 10° crop of a RAW-linear stage with a 0.44 MP output took 2.3 and 3.5 ms against 17.9 and 18.7 ms, and under a full Basic layer 6.2 and 7.5 ms against 52.0 and 54.7 ms (p50, base, new, new, base, at a load of 5.5 to 11, the same SHA-256 each way). Serial and pooled write the same bytes, and the test now asserts that each case pooled through its own gate (`slow_a_pass_between_the_shared_and_its_own_threshold_pools_to_the_serial_bytes`).

`editor-performance --samples 30` measures proxy renders at a 1280 × 800 display bound, where a 3:2 photograph's whole proxy is 1200 × 800 (0.96 MP) and the crop stack's proxy is 2078 × 1386 rendering a 1280 × 719 output: a full Basic layer and a full-strength Presence layer (texture, clarity and dehaze at 100) on each. Release `--locked`, the base `eda010a3` with these rows added (before) against this change (after), in-process on the native Apple M4 Pro, 28 September 2026, holding the host-wide timing lock, in the order before, after, after, before. Each cell is the p50 of each run in that order, then the p95s.

| Proxy render at 1280 × 800 (ms) | Before p50 | After p50 | Before p95 | After p95 |
| --- | --- | --- | --- | --- |
| 24 MP upright, full Basic | 4.23 · 4.43 | 4.37 · 4.79 | 4.54 · 4.55 | 4.55 · 6.27 |
| 24 MP upright, Presence | 64.63 · 68.81 | 13.81 · 15.10 | 66.98 · 72.04 | 20.45 · 20.28 |
| 24 MP crop stack, full Basic | 15.84 · 15.89 | 7.55 · 7.51 | 16.47 · 16.50 | 9.41 · 7.99 |
| 24 MP crop stack, Presence | 58.29 · 58.27 | 50.33 · 52.04 | 63.44 · 63.19 | 56.65 · 60.30 |
| 60 MP upright, full Basic | 5.04 · 5.17 | 4.80 · 7.69 | 5.56 · 5.40 | 5.34 · 11.83 |
| 60 MP upright, Presence | 76.22 · 70.00 | 15.48 · 20.51 | 79.65 · 80.80 | 18.37 · 43.44 |
| 60 MP crop stack, full Basic | 16.80 · 15.91 | 7.82 · 7.89 | 19.02 · 16.50 | 8.42 · 32.70 |
| 60 MP crop stack, Presence | 74.65 · 62.99 | 58.56 · 71.05 | 83.18 · 67.01 | 65.98 · 890.09 |

The 60 MP source (10000 × 6000) fits the bound in 1280 × 768 (0.98 MP) and its crop stack's proxy is 2310 × 1386, rendering 1280 × 720. The 24 MP runs started at a one-minute load of 11.8, 12.6, 14.2 and 14.5; the 60 MP runs at 35.4, 29.9, 27.3 and 25.2, a loaded host, and the second after run is the loaded one (its proxy builds took 26 to 28 ms against 7 to 11 ms in the other three), so its figures are not attributed to the change. Every run's frames, including all four new proxy renders, had the same SHA-256 before and after (`ce95dd3f…` and `8e98cc7e…` for the 24 and 60 MP upright Presence frames, `c8a3deab…` and `73b0eeff…` for their crop stacks with full Basic). The upright Presence render is the change's case: its tiles ran serially below one megapixel and now run on the pool. The crop stack's full Basic render gains from its resample, whose 1280 × 719 output is now pooled; its colour pass, on the 2.9 MP proxy stage, already was. The crop stack's Presence render gains the same resample at 24 MP (58.3 against 50.3 and 52.0 ms); at 60 MP the loaded runs spread wider than that. The upright full Basic render was already pooled by the heavy-colour gate and does not move. The existing 2880 × 1800 proxy rows and the full-resolution rows are unchanged within the runs' spread: at that bound every pass is above the old threshold too. The native scenarios above used the owner's 2880 × 1800 window, whose Fit proxy and 100% regions were above the thresholds that applied before for the passes they ran. One was rerun to check: the 24 MP 100% full-Basic burst (`editor-latency --basic --mode burst --zoom 100`, 360 inputs, regions of 1797 × 1661 rendered at half resolution), before, after, after, before at a one-minute load of 10.2, 12.3, 11.4 and 11.1. It adopted 317, 359, 361 and 360 frames with draft staleness p50 / p95 of 9.00 / 33.92, 8.45 / 16.61, 8.42 / 15.95 and 8.39 / 15.88 ms and no blank or stale photo draws: no change, the first run's tail being its load.

### Desktop slider-to-presented-frame and settled histogram

`editor-latency`, release, warm cache, background evidence launches on the host above, 30 samples
each. This is the desktop measurement the core rows cannot make. **Presented means the desktop's
`Uploaded` message**, recorded as the `preview_displayed` event: the rendered pixels have become a
renderer texture and the canvas draws them from the next frame on. It is **not** display scanout,
which the harness cannot observe, so every figure is an upper bound on the editor's own work and a
lower bound on what an eye sees. The window these runs measure is invisible, so nothing is
composited or scanned out in them at all.

Each measured input is one scripted `slider` step left open, so the step settles only when the
gesture has drained: one input, one `draft.set`, one preview job, one upload, with nothing from the
previous input still in flight. The interval runs from the `slider_draft_set` event to the
`preview_displayed` of the preview generation that `draft.set` produced, correlated by generation
and cross-checked against the draft revision on both ends.

| Interaction (p50 / p95 ms, 30 samples) | 24 MP | 24 MP + 7° crop | 60 MP |
| --- | --- | --- | --- |
| Input to presented frame | 74.8 / 83.4 | 150.3 / 170.2 | 125.0 / 141.3 |
| … of which `draft.set` round trip on the owner | 8.3 / 9.2 | 32.7 / 34.2 | 8.2 / 16.7 |
| … of which render and GPU upload | 66.6 / 75.1 | 117.9 / 143.8 | 116.7 / 133.1 |
| … of which the GPU upload the desktop times itself | 32.5 / 33.6 | 33.4 / 50.9 | 49.7 / 50.5 |
| Final input to settled exact histogram | 99.7 / 107.0 | 184.7 / 213.3 | 185.4 / 208.8 |
| … of which commit to settled histogram | 91.6 / 100.1 | 176.3 / 208.4 | 177.4 / 200.3 |

The settled-histogram rows come from a companion `--mode commit` run of 30 samples, where each step
is a whole gesture moved and released at once, so every sample is one committed frame and its own
exact report. On that build a drafted preview was never analysed — the plot was labelled stale during
a gesture — so the exact histogram was always reduced from the frame the commit's own refresh
rendered, and `analysis_adopted` is the moment that frame and its report were adopted together. The
drag runs measure the same interval once each and agree: 74.8 / 90.3 ms at 24 MP, 239.5 / 248.3 ms
on the crop stack and 182.2 / 200.9 ms at 60 MP, from two commits each.

The crop stack is slower than plain 60 MP at the input-to-frame median because its `draft.set` round
trip is four times longer: the owner replans the crop prefix on every set. Its render and upload are
close to 60 MP's despite a much smaller output stage (5653 × 3180), which is the crop resample's own
interpolating pass.

### Queue cancellation during a gesture

From the same runs, counted out of the event log. Two separate bounds hold.

| Observation | 24 MP drag | 24 MP commit | 60 MP drag | 60 MP commit |
| --- | --- | --- | --- | --- |
| Scripted slider values | 62 | 30 | 62 | 30 |
| `draft.set` requests sent | 32 | 30 | 32 | 30 |
| Preview jobs requested | 32 | 30 | 32 | 30 |
| Preview jobs superseded before display | 2 | 30 | 2 | 30 |
| Commits | 2 | 30 | 2 | 30 |
| Analysis reports adopted | 3 | 31 | 3 | 31 |
| Analysis jobs superseded | 0 | 0 | 0 | 0 |

The first bound is the gesture driver's. A drag step that sends 31 values between two ticks produces
exactly **one** `draft.set` and one preview job: the driver keeps at most one round trip in flight
and only the newest value waiting, so intermediate values are coalesced and never reach the owner at
all. That is why 62 scripted values become 32 requests in the drag runs.

The second bound is the preview queue's. Every commit supersedes the drafted preview of the value it
commits, because the commit's own refresh requests a newer generation before the drafted pixels are
uploaded; the superseded frame is rejected on generation rather than drawn. In the commit runs all
30 drafted previews are superseded this way. In the drag runs only the two releases supersede
anything, because the open steps drain one at a time by construction. **No analysis job is
superseded in any run**: a drafted preview was never analysed, so the only reductions were the ones
belonging to committed frames, and each was adopted with the pixels it was reduced from.

### Memory, scratch and idle with a full Basic layer

`editor-latency --idle` holds one 24 MP image with a Basic layer in which all ten fields are
non-neutral (`exposure 0.5, contrast 25, highlights −30, shadows 30, whites −15, blacks 15,
temperature 20, tint −10, vibrance 30, saturation 15`) and the histogram on — the tools panel is
open by default and the captured state confirms `tools_panel: true` with the plot `ready` and not
stale. RSS is sampled by `ps` about every 50 ms and includes captures, GPU resources and allocator
retention; it is not a CPU-heap figure and GPU memory is not separated.

| Measurement | Result |
| --- | --- |
| Peak RSS, 24 MP, gesture process committing the full Basic layer | 645.3 MiB |
| Peak RSS, 24 MP, second process holding that committed layer | 568.2 MiB, settling to 408.0 MiB |
| Idle CPU, 24 MP with the full Basic layer, 30 s after settling | 1.46% of one core |
| Scratch budget high-water mark, 24 MP colour pass | 14 112 000 B (13.46 MiB) of the 64 MiB target |
| Scratch budget high-water mark, 60 MP colour pass | 13 440 000 B (12.82 MiB) of the 64 MiB target |
| Peak RSS during a 30-input 24 MP latency run (32 window captures retained) | 1336.5 MiB |
| Peak RSS during a 30-input 60 MP latency run (32 window captures retained) | 2143.0 MiB |

The scratch figure is the point of `ScratchBudget::peak`: a reservation is released as soon as its
chunk is done, so `in_use` read from outside a pass is always zero and only the high-water mark says
what the budget carried. It is stable across image size because the colour pass streams bounded row
chunks — at most one megabyte of `[f32; 3]` per Rayon worker — so scratch scales with the worker
count, not the pixel count. The two latency-run peaks are **not** working-set figures: those runs
retain a full-window PNG readback for each of 32 captured frames, which is harness cost, and they are
recorded only so the number is not mistaken for one later.

### Editor process measurements, before and after this work

`measure`, five app-cold launches per workload plus one repeated 60 MP run, on the host above. The
"before" column is the binary built from `3c5def1` (`main` before the Basic panel and histogram,
SHA-256 `ff9eecab…`), measured in the same session minutes apart from the same fixtures, so the two
columns share host conditions. Launch to observed frame is an upper bound: it includes the temporary
background bundle, the executable copy and the harness's capture readback, not scanout.

| Measurement | Before `3c5def1` | After |
| --- | --- | --- |
| Launch to observed frame, empty (median / p95) | 632.9 / 689.7 ms | 671.2 / 751.7 ms |
| Launch to observed frame, 24 MP (median / p95) | 689.3 / 734.7 ms | 734.7 / 742.5 ms |
| Launch to observed frame, 60 MP (median / p95) | 802.6 / 859.0 ms | 808.8 / 857.4 ms |
| Sampled peak RSS (empty / 24 / 60 MP, median) | 143.7 / 489.2 / 967.4 MiB | 133.2 / 466.0 / 975.0 MiB |
| Sampled peak RSS after sixteen 60 MP loads | 1316.0 MiB | 1316.4 MiB |
| Open request to captured frame (24 / 60 MP, median) | 219.7 / 314.1 ms | 222.2 / 314.3 ms |
| … of which GPU upload (24 / 60 MP, median) | 21.3 / 55.8 ms | 21.4 / 52.3 ms |
| Open to full-resolution CPU raster (24 / 60 MP, median) | 172.7 / 239.9 ms | 178.1 / 240.7 ms |
| Idle CPU with a 60 MP image open, 30 s after settling | 0.93% of one core | 1.29% of one core |

Registering the Basic module and the histogram adds no measurable launch cost: the medians differ by
38, 45 and 6 ms across the three workloads with five samples each, the p95 differences are mixed in
direction, and the empty-shell median moves by more than the 60 MP one, which neither a per-image
cost nor a fixed registration cost could produce. Registration of the built-in modules was measured
at 0.18 ms when the registry was introduced, three orders of magnitude below this spread. Peak RSS is lower after the change at the empty and 24 MP workloads and 8 MiB higher at
60 MP, and identical after sixteen 60 MP loads. Both columns sit far above the 233 / 296 / 412 ms
S0 viewer launch medians in the table above, and above the 231 / 285 / 396 ms an earlier `measure`
run recorded for the RAW-era build; because before and after agree here, that gap belongs to the
host and OS state of this session, not to this change, and those older figures should not be
compared against these.

Idle CPU is the one figure that moved: 0.93% before against 1.29% after, one 30-second sample each
with a 60 MP image open. The companion 24 MP run with a full Basic layer measured 1.46%. These are
single samples, and the 500 ms event poll and the window's own redraws are inside all of them, but
the direction is consistent and the histogram plot is now drawn on each of those redraws.

A follow-up gave the histogram plot's canvas program an `iced::widget::canvas::Cache`, held in the
program's own persistent state and keyed by a version the view derives from the render identity and
the `stale` flag (`crates/luxforge-ui/src/widgets/histogram.rs`,
`crates/luxforge-app/src/view/tools_panel.rs::plot_version`), so a redraw with unchanged bins reuses
the tessellated polygons instead of rebuilding three 256-point fills. Measured again on the same host,
binary SHA-256 `db6ce15c…`, one 30-second sample each: **1.32%** with the 60 MP image open
(`measure --binary target/release/luxforge --output artifacts/idle-after --samples 5`) and **1.38%**
on the 24 MP full-Basic-layer workload (`editor-latency --source fixtures/generated/24mp.jpg --idle
--samples 3 --output artifacts/idle-after-basic`), against 1.29% and 1.46% before the cache. Both are
within a single sample's noise of the unfixed figures, not a resolution. The `histogram` smoke
scenario (`artifacts/idle-histogram`) confirms the cached plot still renders and updates correctly.
The likely dominant cost is not this widget: even a `SyncResult::Unchanged` reply to the 500 ms sync
still drives two `Editor::update` calls and two full `view()` rebuilds of every panel every half
second (`crates/luxforge-app/src/app/mod.rs`, `app/tasks.rs`), and re-tessellating three small
polygons twice a second could not plausibly account for the whole 0.3–0.5 point regression on its
own. That path has since been removed: the event sync reads the log only when the owner wakes it
for another client's change ([performance rule 8](../engineering/performance-rules.md#rules)). This is a
measurement to attribute, not a resolved regression, and it is reported as a miss below.

### Instant previews: proxy phase, hop rule and the surface primitive

Native Apple M4 Pro, macOS 26.5.2, Metal, a 2880 × 1800 physical window at 2× scale, release
builds, background hidden-window launches, warm filesystem cache, one-minute load averages between
4 and 7.5 on a shared host. Application SHA-256 `16423973…`. These rows supersede the
slider-to-presented-frame and queue-cancellation tables above, which measured the full-resolution
render-and-upload path that no longer existed at Fit; the histogram, memory and launch rows above
still described that build unless restated here. These rows measured the CPU proxy and exact
phases, which a drag on a machine with a GPU no longer takes ([GPU previews qualified on the M4](#gpu-previews-qualified-on-the-m4)).
Presented here means the update in which the frame became the photo surface's source; it is drawn by the redraw that update requests, the next
frame, and it is still not scanout. The exposure gesture is the same drained drag as above; a full
Basic layer means `--basic`, which commits all ten fields non-neutral first so every frame runs
every colour unit of the module.

| Drained drag, input to presented frame (p50 / p95 ms, 30 samples) | Fit |
| --- | --- |
| 24 MP, exposure only | 9.5 / 29.0 |
| 24 MP, full Basic layer | 18.1 / 24.8 |
| 60 MP, exposure only | 10.1 / 19.1 |
| 60 MP, full Basic layer | 17.2 / 28.9 |
| 24 MP, 7° crop-fit and full Basic layer | 16.1 / 28.7 |

The figures did not depend on the source size, because every frame in a drag was the proxy phase:
a 1716 × 1144 render of the whole recipe against the cached display-bounded proxy of the source
(the photo area of this window at 2×), presented through the surface primitive with no allocation
round trip. Before this work the same 24 MP drag measured 71.8 / 83.2 ms on this host, 60 MP
132.5 / 151.2 and the crop stack 116.0 / 127.6, all exposure only.

| Wild drag, 120 inputs per second for 3 s alternating direction (`--mode burst`) | Presented fps | Staleness p50 / p95 ms | Largest gap ms |
| --- | --- | --- | --- |
| 24 MP, exposure only | 53.7 | 18.1 / 32.9 | 41.8 |
| 24 MP, full Basic layer | 41.7 | 32.9 / 34.1 | 36.1 |
| 60 MP, full Basic layer | 42.5 | 32.7 / 34.1 | 32.4 |

Every one of the 360 scripted values reaches the owner as its own `draft.set` (the gesture's
round trip is synchronous and takes 0.18 / 0.19 ms), the queue kept one proxy job active and one
pending, and every superseded exact phase was cancelled (160, 124 and 124 of them in the three runs).
Before this work the same burst presented one frame in three seconds: every render finished after
a newer job had been requested and was dropped as stale.

| Settled exact histogram after the last input (p50 / p95 ms) | Figure |
| --- | --- |
| 24 MP, exposure only, commit mode, 30 samples | 60.1 / 70.3 |
| 24 MP, exposure only, drag mode, 2 commits | 54.3 / 54.3 |
| 24 MP, full Basic layer, 2 commits | 153.8 / 158.6 |
| 60 MP, full Basic layer, 2 commits | 356.4 / 367.0 |

The settled histogram was the exact phase of the committed frame: the full-resolution render with
every unit, then the reduction. It was not on the input path, so a drag did not wait for it; the
60 MP full-Basic figure misses the 200 ms threshold that was set for 24 MP and is recorded here
because the design asks for the tails.

Where the per-input time went before the last two changes, measured with the per-leg timings the
`slider_draft_preview` event now records: the owner's `draft.set` and preview-job work take under
0.2 ms, the desktop's update, model derivation and view under 0.15 ms together, and every message
handed back into the update loop through the runtime arrived about 7.9 ms later, one frame of the
120 Hz display, because a redraw is always in flight during a drag. With the round trip as a task
and the frame through an image allocation, a 24 MP drag measured 38.6 / 63.0 ms; with the round
trip synchronous, 30.2 / 58.7; with the surface primitive, the rows above.

#### A gesture's press to its first frame

A press opens the gesture's draft with `draft.begin`, and its first `draft.set` cannot go before
that answers. `draft.begin`, `draft.reapply` and `draft.cancel` now run synchronously on the desktop
thread, as `draft.set` does, so the press's first `draft.set` goes in the press's own update; before,
`draft.begin` was an owner task and its answer came back a hop later. Measured with
`editor-latency --mode drag --samples 5` on the 24 MP JPEG at Fit, exposure only, and the same with
`--mask` (the slider bound to a linear gradient mask), on the host and window above (Apple M4 Pro,
macOS 26.5.2, Metal, 2880 × 1800 at 2×), release builds, background launches, warm cache. Both
binaries were driven by the same `xtask`, which reports two rows from the gesture's
`slider_draft_begin` (logged in the update that opens the draft): `press_to_first_draft_set` and
`press_to_first_presented_frame`, the frame of that first set's preview job. A drag has one measured
press per launch (a second, the release step's, adds a `press_to_first_draft_set` sample), so each
row below is 30 or 16 launches. Before is the base build (`8721dac2…`), after this change's
(`3eadae3e…`); the launches ran A, B, B, A in blocks of 15 (8 masked) on a heavily shared host,
one-minute load 14–34, and each block is given separately so the reversal can be read.

| 24 MP drag, p50 / p95 ms | Before (A1 / A2) | After (B1 / B2) |
| --- | --- | --- |
| Press to first `draft.set`, exposure (30 per block, two presses a launch) | 4.34 / 8.79 · 7.19 / 8.95 | 0.05 / 0.06 · 0.05 / 0.08 |
| Press to first presented frame, exposure (15 per block) | 16.45 / 47.31 · 15.93 / 22.19 | 8.07 / 9.41 · 7.77 / 16.68 |
| Press to first presented frame, masked (8 per block) | 16.57 / 18.05 · 16.57 / 17.91 | 9.71 / 25.53 · 9.34 / 9.78 |
| Later inputs, input to presented frame, exposure (75 per block) | 8.74 / 37.58 · 8.67 / 15.89 | 8.73 / 10.51 · 8.60 / 35.09 |

Pooled over both blocks, a press reached its first frame in 16.08 / 42.05 ms before and
7.83 / 9.41 ms after (30 launches each; loads 14.3–27.6 before, 21.5–34.1 after), and 16.57 / 18.05
before and 9.42 / 25.53 after with the mask (16 each). The press now costs what every later input
costs: the drop, about 8 ms at p50, is the one 120 Hz frame the begin's answer waited for, and it
holds in both orders. The first `draft.set` itself left 1.2–9.1 ms after the press before, depending
on whether a redraw was in flight, and 0.04–0.17 ms after. The p95 tails either side are the shared
host: single launches in each block, not one binary, carry them.

The mask shape and crop gestures take the same path and have no `editor-latency` mode of their own.
Their press to first `draft.set`, read from the `mask_draft_begin`/`crop_draft_started` events of
one functional run each of the `mask-panel`, `mask-brush` and `crop-draft` smokes on the small
fixture, with both binaries, was 2.2–9.8 ms before and 0.04–0.07 ms after for five mask shape
presses, and 2.2 and 5.8 ms before and 0.004 ms after for the crop's two starts. Those are single
functional runs, not distributions. A 30-sample `--mode commit` run, which would give one press per
sample, could not be taken when these were measured, because `editor-latency` then generated its
19th value as 0.5700000000000001, which no rail fraction sends; it now generates the decimal the
slider sends.

#### A brush stroke's press

A brush in hand holds no core draft: each stroke's `draft.begin` runs at its press, synchronously,
immediately followed in the same update by the first `draft.set` carrying the press's position.
Before, the empty draft was opened ahead of time, when the brush was put in hand, so the press sent
its first `draft.set` at once. `editor-latency --mode paint` reports the same two press rows drag
mode does, from the stroke's first `mask_stroke_position` (logged in the update that hands the press
to the desktop): `press_to_first_draft_set` and `press_to_first_presented_frame`, the first of the
stroke's own frames presented. Measured on the 24 MP JPEG at Fit with the default 30-position stroke
at 24 ms, on the host and window above (Apple M4 Pro, macOS 26.5.2, Metal, 2880 × 1800 at 2×),
release builds, background launches, warm cache; both binaries driven by this change's `xtask`
through `--binary`. Before is the base build (`0d6626bb…`, commit `8119f891`), after this change's
(`117ff713…`; the committed build differs from it only in the event a brush put down because its
component is gone records, which no stroke reaches). One stroke is one press, so each block is 15 launches and 15 presses; the blocks ran
A, B, B, A back to back on a heavily shared host, with the one-minute load each launch recorded.

| 24 MP paint, p50 / p95 ms | Before A1 (load 14.1–36.1) | After B1 (25.6–34.0) | After B2 (18.5–26.9) | Before A2 (12.0–17.3) |
| --- | --- | --- | --- | --- |
| Press to first `draft.set` (15 per block) | 0.01 / 0.22 | 0.06 / 0.09 | 0.06 / 0.10 | 0.01 / 0.02 |
| Press to first presented frame (15 per block) | 12.13 / 36.90 | 10.58 / 15.65 | 10.66 / 58.73 | 10.42 / 13.00 |
| Later positions, input to presented frame (398–435 per block) | 17.12 / 55.88 | 8.28 / 32.09 | 8.37 / 28.91 | 8.27 / 10.28 |

Pooled over both blocks (30 presses each), the press reached its first `draft.set` in 0.01 / 0.09 ms
before and 0.06 / 0.09 ms after, and its first frame in 10.55 / 30.74 ms before and 10.60 / 18.70 ms
after. Opening the draft at the press costs the synchronous `draft.begin`, about 0.05 ms at p50, and
the press's first frame is unchanged in both orders: the difference is well under one 120 Hz frame
and inside the spread between the two blocks of the same binary. The p95 tails are single launches
on the shared host (B2's 58.7 ms is one launch; A1 ran at loads up to 36), not one binary, and the
later-position rows, whose path this change does not touch, move with the load in the same way.

| Core diagnostic, 24 MP crop stack (p50 / p95 ms, 10 samples) | Full resolution | Proxy for 2880 × 1800 |
| --- | --- | --- |
| Same stack without colour | 29.1 / 32.9 | 16.9 / 19.0 |
| One +1 EV Basic layer | 35.8 / 37.5 | 21.1 / 23.0 |
| Full Basic layer | 77.3 / 79.7 | 46.9 / 48.0 |
| Proxy build (a cache miss: once per source, bounds and window size) | — | 21.2 / 25.7 |

When these rows were taken the proxy source for this stack was the whole 4677 × 3118 proxy stage,
larger than the 2879 × 1618 output it produces, because the 16:9 crop discards most of the rotated
stage; the colour pass covers only the band of rows the crop reads, which is what brought the
full-Basic rows down from 162.5 and 99.8 ms in the first measurement of this plan. A cropped
stack's proxy source later held only the window of the proxy stage its crop reads (below), until the
GPU-first stage 5 deleted the proxy's windows. The full-Basic proxy render was the largest per-input
cost on that path.

#### A tight crop's windowed proxy

A tight crop fits a small output into the bounds, which raises the proxy scale towards one; on the
after build the proxy held only the window of that proxy stage the crop reads. The CPU proxy now
holds its whole proxy stage and draws a drag only in a session without a GPU
([instant previews](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu)). Measured on
the X100VI with a 1801 × 1574 crop of its 7728 × 5152 stage, in the 1716 × 1576 bounds of a
default hidden window, then Presence committed at Clarity 30, Dehaze 20, Texture 20 and then Clarity
60: one evidence-script launch per run of each release binary from a background-only bundle with
an isolated catalog, RSS sampled every 50 ms by an ad hoc script that is not a committed harness;
times are from the run's own `script_step`, `preview_displayed` and `analysis_adopted` events. Native Apple M4 Pro, macOS 26.5.2, runs back to back and
reversed (before, after, after, before), one-minute load average 23.5–28.2 throughout with other
sessions building; single launches, not distributions. Application SHA-256 `a2f1d98d…` before and
`ca82c5b9…` after.

| Per run (before 1, before 2 · after 1, after 2) | Before | After |
| --- | --- | --- |
| Proxy source, crop alone | 7363 × 4909 | 1716 × 1500 |
| Proxy source under Presence | 7363 × 4909 (414 MiB of planes) | 2437 × 2530 (71 MiB) |
| Crop commit, request to first frame | 151, 134 ms | 43, 119 ms |
| First Presence commit, request to proxy frame | 8961, 8338 ms | 1888, 2097 ms |
| First Presence commit, request to settled report | 18203, 17147 ms | 11116, 11338 ms |
| Clarity change, request to proxy frame | 8279, 7490 ms | 1289, 1229 ms |
| Clarity change, request to settled report | 17462, 16065 ms | 11129, 10350 ms |
| Sampled peak RSS | 2914, 2914 MiB | 2074, 2072 MiB |

The proxy phase itself (`render_ms`) was 8.3–8.9 s before and 1.2–2.1 s after for the Presence
commits; the first commit after included the exact stage's one Dehaze reduction, which the proxy
phase then prepared and the exact phase read from the estimate store. What remained of a commit was
the exact phase: a Presence render over the whole 40 MP stage, unchanged, which was then the 100%
view, histogram and export source. The figures filed with the bug (about 8.7 s per commit and 3.9 GiB
peak) came from another journey; these runs compare one journey before and after, on one host.

#### A crop draft's input stage at Fit

On the after build a crop draft's input stage was a display-size proxy of the layers before the crop
at Fit, and the exact stage was rendered only at a percentage zoom that needed it. Before, the stage
was the whole prefix rendered at full resolution and uploaded into a texture of its own size. The
stage at Fit is now the GPU's picture at rest of the prefix, with the reference's exact frame reduced
to the view where the GPU cannot draw it
([instant previews](../design/instant-preview.md#a-crop-drafts-input-stage)). Measured
with `editor-latency --mode crop-start --presence` (a Presence layer with Texture, Clarity and Dehaze
at 100, then per sample a crop draft Start held open 1.5 s, Cancel and 1.5 s more), at Fit in the
hidden 2880 × 1800 window at 2×, on the generated 60 MP JPEG (10000 × 6000, 5 Starts a launch) and
the owner's X100VI RAF (7728 × 5152, 4 a launch). Native Apple M4 Pro, macOS 26.5.2, Metal; release
builds, background launches, warm cache; before is the base (`800f09e7…`), after this change
(`f1e3c268…`), both driven by the same `xtask`, launched back to back as before, after, after,
before. The host was shared: one-minute load 17.7, 18.8, 17.7, 16.0 for the 60 MP launches and
13.8, 18.4, 21.1, 26.5 for the X100VI. The open time is from the Start step's `script_step` to the
`frame_captured` the editor takes once the stage is on screen, so it includes the window readback.
Memory is the Performance section's `resources.read` figure and GPU its allocated bytes, read at
the end of each hold; the baseline is the settled frame before the first Start.

| Per launch (before 1, before 2 · after 1, after 2) | Before | After |
| --- | --- | --- |
| 60 MP: Start to `crop_draft_started`, p50 | 0.11, 0.10 ms | 0.14, 0.36 ms |
| 60 MP: Start to the stage on screen, p50 (min–max) | 2832 (2781–8068), 5065 (3044–9589) ms | 65 (61–98), 70 (63–80) ms |
| 60 MP: stage texture | 10000 × 6000, 229 MiB | 1716 × 1030, 6.7 MiB |
| 60 MP: GPU allocated while the draft is open (baseline) | 450.8, 450.7 MiB (211.2, 211.1) | 222.0, 222.1 MiB (211.1, 211.3) |
| 60 MP: memory while the draft is open, p50 (baseline) | 2157, 2122 MiB (1490, 1468) | 1824, 1822 MiB (1468, 1478) |
| 60 MP: sampled peak RSS and process CPU of the launch | 1668, 1641 MiB; 265, 270 s | 1425, 1433 MiB; 135, 136 s |
| X100VI: Start to `crop_draft_started`, p50 | 0.12, 0.13 ms | 0.09, 0.09 ms |
| X100VI: Start to the stage on screen, p50 (min–max) | 2013 (1851–3237), 2214 (2031–9334) ms | 354 (138–1033), 322 (291–550) ms |
| X100VI: stage texture | 7728 × 5152, 152 MiB | 1716 × 1144, 7.5 MiB |
| X100VI: GPU allocated while the draft is open (baseline) | 369.8, 369.8 MiB (211.2, 211.2) | 222.3, 222.3 MiB (211.2, 211.2) |
| X100VI: memory while the draft is open, p50 (baseline) | 2593, 2736 MiB (2386, 2430) | 2552, 2460 MiB (2416, 2497) |
| X100VI: sampled peak RSS of the launch | 2323, 2316 MiB | 2171, 2256 MiB |

`crop_draft_started` is logged in the Start's own update on both builds, since `draft.begin` became
synchronous, so the draft's section and keys are live at once either way; what changed is how long
the frame waits for its stage. Both orders agree: the 60 MP stage opens 40 to 70 times sooner and
the X100VI's about 6 times, and the GPU allocation a draft adds falls from 240 and 158 MiB to 11 MiB
(the stage's own texture is 7 MiB of it). The X100VI's proxy costs more than the JPEG's because the
Presence prefix renders through the RAW development's linear planes. The process memory rows are
noisy on this host; the 60 MP ones fall by about 300 MiB, the full-resolution frame the stage no
longer renders, and the X100VI ones overlap. The launch CPU halves at 60 MP because no Start on the after
build rendered the prefix at full resolution.

Aliasing at Fit was settled on a generated 6000 × 4000 zone plate (a radial chirp reaching 0.5
cycles per pixel in the corners, JPEG quality 95 without chroma subsampling): the crop draft opened
at Fit with each binary through `editor-latency --mode crop-start --samples 1`, and the captured
stage compared, over the 77% of it whose source frequency lies beyond the display's Nyquist limit,
with a linear-light box downscale computed independently. The exact stage drawn by the surface's
bilinear sampler without mip levels aliases: full-contrast replicas of the centre rings across the
frame, a standard deviation of 63.6 codes there and a mean of 145.1, 16 codes darker than the true
average. The proxy stage does not: it shows only the faint residual rings of a box filter, 15.8 codes
against the reference's own 10.9, with a mean of 160.3 against 160.8. A percentage zoom draws the
exact stage at or above its size, where the sampler magnifies and cannot alias.

RAW, one functional trial per camera through `raw-editor` (the same 13-step journey as the RAW
tables below, so these are single launches and not distributions): request to display of the
exposure step is 10.5 ms on the Z6, 14.6 ms on the X100VI and 15.0 ms on the Air 2S, against
133.3, 178.8 and the Air 2S figures recorded below for the full-resolution path; rotate, crop-fit
and undo present in 18 to 46 ms. The white-balance steps still take 354–364 ms on the Z6 and
1364–1428 ms on the other two, because a committed temperature, tint, gain or neutral pick
redevelops the mosaic on the source worker before its exact frame exists; that is the release cost
listed in the [performance rules](../engineering/performance-rules.md#known-remaining-costs).

A drafted RAW temperature or tint previews approximately on the developed planes
([instant previews](../design/instant-preview.md#a-raw-white-balance-during-a-drag)), so its drag
has a frame per input like exposure's. `editor-latency`, drained drag of 30 inputs at Fit, same
M4 Pro host (macOS 26.5.2, release build, warm cache), input to presented frame p50 / p95: Z6
temperature 11.2 / 21.0 ms (load average 2.4–4.1) and tint 10.9 / 19.3 ms (2.9–6.1), X100VI
temperature 10.5 / 20.6 ms (6.7–7.0) and tint 13.6 / 20.4 ms (5.2–6.2), against RAW exposure at
12.0 / 19.9 ms on the Z6 (6.1–6.7) and 11.9 / 21.3 ms on the X100VI (5.8–6.6). Every drafted value
produced a frame labelled approximate and none was analysed. The release still waits for the
redevelopment: release to the committed frame is 534–540 ms on the Z6 and 1578–1639 ms on the
X100VI in those runs (two commits each), and in `--mode commit`, 30 commits per run with other
agents building, 376 / 430 ms p50 / p95 for Z6 temperature at load average 11–13 (436 / 465 ms at
23), 441 / 1458 ms for Z6 tint at 23–28 and 1516 / 3175 ms for X100VI temperature (15 commits, load
average 20–22), against 34.5 / 120.8 ms for RAW exposure on the Z6 at 11–13. A wild drag (`--mode
burst`, 360 values over 3 s) presents 43.2 frames per second with a staleness of 16.5 / 34.7 ms
p50 / p95 on the Z6 temperature slider and 35.7 at 16.6 / 30.6 ms on the X100VI's, against 48.8 at
16.7 / 40.2 ms for RAW exposure on the Z6 (load average 5.9–7.4); an earlier RAW exposure burst on
the Z6 presented 45.0 at 16.3 / 37.4 ms, against 51.0 and 16.8 / 30.4 ms for Basic's Exposure on
the same file in the run after it, both at load average 13.

Memory and idle from the same timing tier, five launches per workload: sampled peak RSS 130.4 MiB
empty, 389.7 MiB at 24 MP and 808.8 MiB at 60 MP (medians); idle CPU 1.53% of one core over 30 s
with the 60 MP image open, a miss of the 1% target in the same range as the 1.29–1.46% recorded
before this work, with the histogram and the surface primitive both drawn on each redraw and the
500 ms sync still in place; no timer was added and none remained for previews or gestures on that
build.

#### A minified After side at Fit

Before/After keeps the exact raster it displays as its After side, even at Fit, so the photo surface
drew a full-resolution texture through its bilinear sampler with no mip levels at well under half
its size, the case the crop draft's zone-plate measurement showed aliasing. It was confirmed before
any change and then removed by giving such a texture linear-light mip levels
([gpu previews](../design/gpu-preview.md#mipmapped-minification),
[before and after](../design/before-after.md#after-at-fit)).

The generator and script of the crop-stage measurement were not kept in the repository, so the
judgement was rebuilt from its description and is now the `compare-zone-plate` smoke scenario, whose
statistics (`xtask/src/zone_plate.rs`) are the ones named above: the capture's mean and standard
deviation of luma over the pixels of the drawn photograph whose source frequency lies beyond the
display's Nyquist limit, beside those of an independent area-weighted box average of the decoded
source's linear light, re-encoded. The fixture is a generated 6000 × 4000 zone plate (`zone-plate.jpg`
from `cargo xtask generate-fixtures`): `round(127.5 + 127.5 cos(pi k r^2))` with `k` such that the
frequency reaches 0.5 cycles per pixel at the corners, JPEG quality 95 without chroma subsampling.
Its region beyond Nyquist is 86.1% of the photograph at Fit (1,689,852 of 1716 × 1144 pixels), not the
77% the earlier plate had, so the reference's own standard deviation there is 18.1 codes where that
plate's was 10.9, and the figures below are comparable with each other but not number for number with
those.

Scope: native Apple M4 Pro, macOS 26.5.2, Metal; release builds launched hidden in the background
bundle at 1440 × 900 logical, 2× (2880 × 1800 physical); the fixture opened at Fit, left 4 s for its
exact render to land, then compared with `\` and After alone revealed (`Position(0.0)`), judged on the
captured frame. The host was shared (one-minute load average 12 to 19). Before is the base
(`46a85159`) with only the evidence state's `compare_after` field added; after is this change. Both
ran through the same `xtask`.

| 24 MP zone plate at Fit, 1716 × 1144 | Before | After |
| --- | --- | --- |
| Display proxy before the comparison (control): standard deviation, mean | 18.06, 159.98 | 18.06, 159.98 |
| Reference box reduction: standard deviation, mean | 18.05, 159.99 | 18.05, 159.99 |
| After alone: standard deviation, mean | 64.03, 144.67 | 12.38, 160.55 |
| After alone: standard deviation against the reference's, mean against its | 3.55 times, 15.32 below | 0.69 times, 0.57 above |
| Resident photo-slot bytes with the comparison open | 167,772,160 | 144,776,244 |
| Mip levels resident, mip chains generated | not reported | 31,999,028 bytes, 1 |

The aliased reading reproduces the earlier one (63.6 and 145.1): full-contrast replica rings across
the frame and a mean 15 codes below the true average. With mip levels the frame shows only faint
residual rings; its standard deviation is below the reference's because the box-filtered levels,
read trilinearly, are softer than the single box reduction. The After texture is now 6000 × 4000 with
its 13 levels, 96,000,000 bytes of base and 31,999,028 of levels, which the resident bytes include;
the plain texture it replaces reserved a 6144 × 6144 bucket of 150,994,944 bytes. The display-size
photograph holds no levels, before the comparison and after it ends.

The generated 60 MP JPEG (10000 × 6000) is held in two tiles by the device's 8192 pixel textures and
cannot have levels, so there After draws the display reduction at Fit (`compare-tiled`): After alone
equals the display-size photograph drawn before the comparison to the code (largest channel
difference 0, against 129 before), and the resident photo-slot bytes with the comparison open fall
from 256,825,216 to 33,554,432, since the 240 MB tiled texture is no longer drawn at Fit.

The `workspace` scenario, which holds the Before/After checks for crop and orientation alignment,
endpoints, holds, zoom alignment and unchanged divider uploads, passes with 62 pixel claims held and
none failed. Not measured: the GPU time of a chain's passes and their encoding time on the UI
thread, a Fit drag or window resize while comparing, and any native run off this host; the
headless tests run on the same adapter.

#### The proxy build's memory and time

The proxy's box downscale runs in bands of output rows, each worker holding one intermediate of
about 1 MiB, where it held one intermediate of every source row the window reads
([instant previews](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu)). Measured with
the ignored `measure_the_proxy_build_on_photo_sized_sources` test, now in `cpu_proxy/tests.rs`: one process per
run decodes the generated JPEG, then builds its proxy for the owner's 2880 × 1800 display bounds 30
times, and `/usr/bin/time -l` reports the process's maximum resident set size. The same run with
`LUXFORGE_PROXY_BUILDS=0` decodes and builds nothing, which is the floor the build's peak is read
against. Native Apple M4 Pro (14 cores), macOS 26.5.2, Rust 1.94.0, release test binaries of the
base commit (`ec132e71`, with the same measurement test) and of this work, warm filesystem cache.
Two rounds, before-after-after-before then after-before-before-after, one-minute load average
13.4–13.7 and 11.6–12.6 with other sessions building. Both builds' proxies hash to the same bytes at
both sizes.

| Maximum resident set size (MiB) | 24 MP, 6000 × 4000 → 2700 × 1800 | 60 MP, 10000 × 6000 → 2880 × 1728 |
| --- | --- | --- |
| Decode only, both builds | 102.2–102.4 | 240.8–240.9 |
| Before, four runs | 246.1–246.2 | 459.1–459.3 |
| After, four runs | 148.1–155.4 | 290.8–294.7 |
| The build above the decode floor, before / after | 143.9 / 45.9–53.2 | 218.3 / 49.9–53.8 |

Before, the build's peak is the whole-window intermediate (123.6 MiB at 24 MP, 197.8 MiB at 60 MP)
plus the 18.5 or 19.0 MiB proxy; after, it is the proxy plus the pool workers' band intermediates
and the allocator's retention of them, and it no longer grows with the source. Peak footprint
(`/usr/bin/time -l`'s other figure) moves the same way: 238.3 to 141.2 MiB at 24 MP and 451.4 to
283.0 MiB at 60 MP in the first round.

| Build time, 30 builds per run (ms) | Before | After |
| --- | --- | --- |
| 24 MP, first build of the process | 13.2, 13.0, 14.3, 47.8 | 7.9, 7.7, 12.0, 13.9 |
| 24 MP, p50 | 8.1, 8.2, 7.9, 54.8 | 7.4, 7.1, 8.3, 14.5 |
| 60 MP, first build of the process | 18.2, 18.1, 21.1, 20.3 | 14.2, 13.5, 20.2, 13.2 |
| 60 MP, p50 | 14.5, 16.6, 16.2, 15.5 | 13.0, 15.3, 16.4, 13.2 |

Each cell lists the first round's two runs, then the second round's two, in run order. The first
build of a process, which touches its buffers for the first time, is lower after in every pair of
both rounds; the steady p50 is lower after in the first round and not in the second, so no change
in the steady build time is claimed. The second round's second pair at 24 MP ran through a load
burst (before p95 162.8 ms, after 43.5 ms) and is kept for completeness. On the after build a job with a proxy phase
also compiled its stack once at the proxy stage, where the plan and the render had each compiled it;
that saved one `O(layers)` compile per proxy-phase job and was not separately timed.

#### The workspace derivation per message

The desktop derives every region of the workspace — title bar, state panel, canvas, Masks panel,
tools panel, histogram, status bar, palette and the Performance section — after every message, with
no per-region keys ([develop workspace](../design/develop-workspace.md#message-flow)). What
that costs is the `detail.loop.last_rederive_ms` of `slider_draft_preview`: the `rederive` span of
the update before that event's own, with a capability section's second derivation and the visible
curves' sample requests inside it; `last_view_ms` is the `view()` of the same update. One sample per
drafted input. Native Apple M4 Pro (14 cores), macOS 26.5.2 (25F84), Metal, a 2880 × 1800 window at
2×, background hidden-window launches of release builds, `--locked` (Cargo.lock `a2642772…`), warm
filesystem cache, the generated 24 MP and 60 MP JPEGs at Fit, Basic's exposure slider, and with
`--mask` the same slider bound to a linear gradient. Three builds of the same tree: keyed (the
region keys, `Tracked` stamps and per-section digests, SHA-256 `415770e3…`), bypassed (the same with
every region key treated as moved, so every region is derived on every message while the tools
panel's per-section digests still decide which sections are rebuilt, `6a16c4a0…`) and this change's
(`814ee788…`, every region derived with no key or digest and the palette's entries built only while
it is open). `editor-latency --mode drag` (30 inputs, 62 `slider_draft_preview` events a launch)
and `--mode burst` (360 events a launch), each workload run keyed, other, other, keyed back to back,
so every figure below pools two launches: 124 samples a drag cell and 720 a burst cell. The host
was shared with other agents building throughout; the one-minute load at each round's start is
given. Nearest-rank p50 / p95 in ms, `last_rederive_ms` then `last_view_ms`.

| Drag, keyed against bypassed | Keyed | Bypassed |
| --- | --- | --- |
| 24 MP, load 21–33 | 0.017 / 0.156 · view 0.066 / 0.089 | 0.082 / 0.178 · view 0.064 / 0.077 |
| 60 MP, load 21–33 | 0.025 / 0.099 · view 0.069 / 0.091 | 0.087 / 0.182 · view 0.066 / 0.090 |
| 24 MP masked, load 21–33 | 0.030 / 0.235 · view 0.102 / 0.220 | 0.101 / 0.156 · view 0.098 / 0.130 |
| 24 MP, repeated, load 11–15 | 0.030 / 0.261 · view 0.068 / 0.177 | 0.137 / 0.429 · view 0.072 / 0.152 |
| 60 MP, repeated, load 11–15 | 0.017 / 0.130 · view 0.065 / 0.088 | 0.081 / 0.143 · view 0.067 / 0.083 |
| 24 MP masked, repeated, load 11–15 | 0.017 / 0.100 · view 0.088 / 0.105 | 0.123 / 0.355 · view 0.106 / 0.249 |

| Burst, keyed against bypassed, load 19–43 | Keyed | Bypassed |
| --- | --- | --- |
| 24 MP | 0.003 / 0.018 · view 0.078 / 0.227 | 0.110 / 0.322 · view 0.090 / 0.204 |
| 60 MP | 0.005 / 0.027 · view 0.095 / 0.326 | 0.120 / 0.484 · view 0.143 / 0.670 |
| 24 MP masked | 0.007 / 0.028 · view 0.185 / 1.084 | 0.117 / 0.416 · view 0.168 / 0.765 |

| This change against keyed | Drag, load 14–18: keyed · this change | Burst, load 21–37: keyed · this change |
| --- | --- | --- |
| 24 MP | 0.052 / 0.192 · 0.116 / 0.270 | 0.008 / 0.029 · 0.109 / 0.580 |
| 60 MP | 0.021 / 0.120 · 0.086 / 0.143 | 0.006 / 0.026 · 0.063 / 0.189 |
| 24 MP masked | 0.028 / 0.368 · 0.095 / 0.219 | 0.007 / 0.026 · 0.048 / 0.157 |

Deriving every region costs about 0.05 to 0.14 ms at p50 per message, against 0.003 to 0.05 ms with
the keys. The p95 follows the host rather than the build: in drags the keyed build's own p95
reached 0.24 to 0.37 ms in three of the nine keyed cells, and a launch whose derivation p95 is high
has its `view()` inflated with it (the bypassed repeated 24 MP pair's second launch: derivation
0.160 / 0.523, view 0.152 / 0.216, against 0.124 / 0.269 and 0.065 / 0.082 for the first). The
quietest launches of this change's burst held p95 at 0.126 ms (60 MP) and 0.082 ms (masked); the
busiest reached 0.68 ms. Bursts separate the builds cleanly, because between two paced inputs the
keyed build derives nothing, and there the bypassed p95 passed 0.2 ms in every launch. Input to
presented frame is unchanged by any of it: p50 7.7 to 9.5 ms for every build in every round, one
frame of the 120 Hz display.

Where the time goes, from a diagnostic build of this change that timed each region (not kept;
drags and a burst of one launch each, load 28–31): outside a mask the tools panel takes 64 to 67%
and dropping its previous model 7 to 11% more, and the Masks panel 16 to 21%; in the masked drag the
tools panel takes 44% (and 8% to drop) and the Masks panel 43%; the canvas 1 to 7%; every other
region 3% or less, the palette 0.1%. The tools panel's time is
spread over its sections by the controls they build: in a burst the Presets section 33%, the colour
mixer 32% and Basic 20%; in the masked drag Basic 62% and the mixer 34%. The tools panel reads
almost every input the editor holds, so no one value keys it without the input tracking the keys
were built on, and during a drag it is derived for every input anyway. The decision this informed
is recorded in [decisions](../decisions.md#post-consolidation-review).

### Native viewport-region qualification

These figures qualified the CPU viewport path at 100% and above — the half-scale motion region, the
exact visible-region refinement after the shared quiet timer and the whole frame settled behind it —
which the GPU-first stage 5 deleted; the GPU now draws a drag and the picture at rest over the visible
region ([GPU previews](../design/gpu-preview.md#at-100-and-above)). They stay as the record of that
build. The qualified release executable was SHA-256
`888c0313049c7c590311b6dfba4fd60edd40b0c39e780c1147ae24561b5ba998`. Native
qualification uses the Apple M4 Pro, Metal and hidden background launches on generated 24 MP
(6000 × 4000) and 60 MP (10000 × 6000) JPEGs. A presented frame is a correlated
`preview_displayed` adoption, not display scanout. Photo-surface write counters measure uploads
issued during draw encoding; backend-owned staging remains unmeasured. The final timing commands
ran serially after functional work. Their host's one-minute load is recorded at each command's
start and end; a result starting above 8.0 is a loaded-host diagnostic, not a clean baseline.
Case JSON and command loads are retained locally under `artifacts/review-fixes/`, including
`final-timing-runs.json`; the native scenario results are under `rendered-qualified/` there.

That build's `viewport-region`, `viewport-fallback` and `viewport-idle-fit` scenarios exercised
state, event, pixel and GPU invariants over a generated 24 MP JPEG. All three passed on this
executable in the native rendered tier, which passed 33 smoke scenarios. Each recorded zero blank
photo draws and zero stale photo draws. Peak photo residency was 182,255,616 bytes in the region
journey and 150,994,944 bytes in each fallback and idle-Fit journey. The idle-Fit capture became
ready without another input and reported expected full version 5 drawn as version 5, with two
drawn frames from the view change to its idle deadline, before capture was allowed. Zero blank
draws means each attempted photo draw showed some photograph pixels; it does not prove that every
canvas pixel was covered. The focused Metal surface tests drive actual `write`, `write_region`,
`prepare`, `draw` and readback: all three Appendix A blank-photo regressions pass, bucketed
single- and multi-tile readbacks match, stale overlays are suppressed,
and a deferred photo becomes current after retirement without another user input. The final focused
surface run passed 31 tests; its two ignored retirement timing diagnostics were run separately
after functional work. The full-photo bound of that build permitted each photo surface a current and retiring allocation up to
512 MiB each, plus two region sets up to 32 MiB each, all surfaces together within the 1088 MiB
ceiling; the 1088 MiB temporary-overlap ceiling was provisionally accepted by the owner on 2026-09-27 for photo textures only.
The region sets are gone with the region path, and the photo bound is now a current and a retiring
allocation of at most 512 MiB each within 1 GiB. It is not a total editor or GPU product budget. Crop GPU textures and tiles, overlays and backend staging sit outside that photo accounting and are not fully measured or bounded; native accounting and bounds remain required before any total-memory guarantee.

The final 24/60 MP Fit exposure drags each had 30 inputs in one release launch. Input to presented
adoption was 8.34 / 8.95 ms p50 / p95 at 24 MP and 8.57 / 8.80 ms at 60 MP. The runs started at
one-minute load 7.72 and 7.34, respectively. These are exposure-only journeys, not matched
before/after comparisons with the full-Basic Fit pairs below.

Final 100% and 200% bursts scripted 360 inputs over three seconds. The values below count
`preview_displayed` adoptions, which can outnumber actual screen scans. Every burst recorded zero
blank and zero stale photo draws, but all three started above the 8.0 load threshold:

| Burst | Adoptions | Draft staleness p50 / p95 ms | Adoption gap p50 / p95 ms | Load at start → end |
| --- | ---: | ---: | ---: | ---: |
| 24 MP, 100%, full Basic | 331 (330 regions + one whole) | 8.64 / 25.69 (329 samples) | 8.43 / 17.33 (328) | 10.51 → 9.75 |
| 60 MP, 100%, full Basic | 359 (358 regions + one whole) | 8.49 / 16.77 (357) | 8.31 / 9.65 (356) | 9.75 → 10.25 |
| 60 MP, 200%, masked 7° crop and moving pan | 158 (157 regions + one whole) | 25.66 / 34.47 (153) | 17.19 / 25.60 (152) | 10.25 → 9.75 |

The final held viewport journeys each adopted seven region frames and then a whole-image exact
report. They are individual journeys, not latency distributions. Each had zero blank photo draws;
the four or five stale draws are the explicitly marked coherent fallback while a replacement is
pending. No settled pan added a photo write. Each started above load 8.0:

| Journey | First region after input | Stale draws | Peak sampled process RSS | Photo writes across settled pan |
| --- | ---: | ---: | ---: | ---: |
| 60 MP full Basic, mask, 7° crop at 100% | 26.00 ms | 5 | 1394.7 MiB | 16 → 16 |
| 60 MP Dehaze at 100%, cold estimate | 863.91 ms | 4 | 1681.2 MiB | 14 → 14 |
| Nikon Z6 RAW exposure at 100% | 17.70 ms | 4 | 1077.8 MiB | 13 → 13 |
| Fujifilm X100VI RAW exposure at 200% | 8.93 ms | 4 | 1616.9 MiB | 13 → 13 |
| DJI Air 2S RAW exposure at 100% | 8.73 ms | 4 | 1044.8 MiB | 13 → 13 |

The cold Dehaze first region includes its exact whole-stage estimate and is still the visible
outlier. Sampled process RSS includes other editor and capture work and is not the photo-texture
charge or a separate GPU allocation measure. A final `measure --samples 1` run recorded one
cold-launch peak RSS
of 379.9 MiB at 24 MP, 636.1 MiB at 60 MP and 912.2 MiB after sixteen 60 MP loads. These are
single launches, not memory distributions. Its 60 MP idle window used 1.421% of one core over
30.26 seconds with the Performance section open, missing the provisional <1% target. That is one
window, not a p95 or an idle distribution.

The two ignored Metal retirement diagnostics ran after functional work. On the pre-fix test
executable, an empty render-thread submit waited 57.075 ms during a 63.103 ms GPU workload,
against a 25.542 µs
control submit; the diagnostic failed as expected. On the final source, the same probe passed:
5.125 µs with one retirement in flight during a 63.389 ms workload. Fifteen smaller retired
workloads produced submit waits of roughly 0.905–3.560 ms before and 2.167–25.667 µs after.
Each trial asserted a real retiring texture. These are timing diagnostics on a shared host, not a
CI timing threshold.

The final 24 MP `editor-performance` pair used 30 samples per build. The 10° crop stack's 200
transforms were 29.65 / 32.96 ms p50 / p95 before and 29.54 / 33.79 ms after; the full-Basic
colour stack was 68.72 / 73.25 → 70.39 / 77.63 ms. On the final 60 MP build, those workloads
were 69.49 / 75.26 and 166.91 / 176.39 ms. The 24 MP legs started at loads 2.15 and 5.46;
the 60 MP leg started at 6.93 and ended at 10.51. The changes do not establish a core speedup.

The older matched Fit comparisons are retained to show the with-and-without-deferral measurement
that qualified the drafted exact-phase deferral. They were one release launch per condition with 30 scripted inputs on a shared
host and predate the current surface changes. Values are input-to-adoption p50 / p95 milliseconds:

| Journey | Before deferral | After deferral |
| --- | ---: | ---: |
| 24 MP Fit drag | 10.49 / 14.38 | 8.59 / 8.86 |
| 60 MP Fit drag | 15.34 / 17.86 | 8.56 / 8.75 |
| 60 MP Fit masked paint | 10.17 / 17.04 | 8.16 / 11.17 |

The same pairs recorded **slower** release-to-settled-histogram times: 24 MP 32.48 / 35.42 →
33.23 / 62.72 ms and 60 MP 48.40 / 51.83 → 62.03 / 73.00 ms (p50 / p95; only two releases per
condition, loaded host). These are diagnostics, not a stable release distribution. The older 24 MP
`editor-performance` after run also started at one-minute load 8.43, above the 8.0 threshold; its
10° crop stack measured 31.94 / 39.17 → 28.93 / 34.49 ms p50 / p95 over 30 samples per build,
under that loaded-host caveat. Earlier native crop-window evidence recorded a 451 × 418 requested
intermediate; the committed 1000 × 800 fixture proves bounded materialization, and the prior core
window run counted 14 passed and one ignored test. These older figures do not qualify this build.

The original 2026-09-26 moving-frame RAW white-balance criterion passed for the Nikon Z6 and Fujifilm X100VI on this final executable but failed for the DJI Air 2S: release residuals were 2.42%, 3.69% and 17.33%, respectively, against 10%. The owner revised the 100% criterion on 2026-09-27 to apply the 10% relative-to-adjustment gate to a held full-detail approximate draft after the shared 120 ms quiet refinement and before release. The earlier held comparisons were 0.81%, 1.39% and 5.06%, respectively, measured with a one-second hold under the earlier harness. Fresh native `raw-panel` runs of the revised verifier pass for the Z6, X100VI and Air 2S: 100% held residuals are 0.805669%, 1.387562% and 5.056605%, respectively; Fit moving residuals are 0.293894%, 0.512237% and 3.047137%, with mean differences of 0.017890, 0.048140 and 0.345406 codes. Each passes its 10% relative gate, and each Fit mean stays below one code. The journeys also complete the later crop and placement checks. Evidence is in `artifacts/accepted-preview-decisions/raw-{z6,fuji,air}/result.json` and corresponding `app/raw-panel-checks.json`; `raw-runs.json` records the commands and binary identities. The moving half-detail figures remain recorded; motion then had separate viewport identity, approximate-label, visual-response and refinement checks, without a numeric softness threshold, and Fit its 10% relative and one-code mean gates. The GPU's white-balance drag is measured [below](#a-raw-white-balance-drag-on-the-gpu-against-its-release). Neither comparison defines a general numerical photo-error bound.

### Provisional targets: measured

These provisional targets mostly retain earlier Fit and editor-wide workload evidence; the final
single-window idle result is updated below. They document those checks only; the viewport
half-detail path and shared quiet timer, since deleted, and the exact-estimate costs are recorded separately above. A
miss is a finding for the owner's review, not a blocker. The GPU-first editor's figures for the same
targets, taken on 2026-10-06 on a quiet host, are in [the timing tier](#the-timing-tier): the slider,
settled histogram, burst and memory targets pass, and idle CPU, launch and the uncached 24 MP open miss.

| Provisional target | Measured | Verdict |
| --- | --- | --- |
| Warm 24 MP slider-to-presented-frame p95 below 16 ms, acceptable below 32 ms | 29.0 ms p95 (9.5 p50, 30 samples); 24.8 ms p95 with a full Basic layer | **Acceptable** (measured against the earlier 100 ms threshold; below the 32 ms bound, not the 16 ms target) |
| Instant preview: drained drag p95 ≤ 33 ms at Fit, 24 and 60 MP, full Basic layer, with and without a 7° crop | 24.8, 28.9 and 28.7 ms p95 (30 samples each) | **Pass** |
| Instant preview: burst drag ≥ 30 presented frames per second | 53.7 (exposure), 41.7 and 42.5 (full Basic, 24 and 60 MP) | **Pass** |
| Instant preview: burst staleness p95 ≤ 50 ms | 32.9, 34.1 and 34.1 ms | **Pass** |
| Instant preview: RAW exposure step presented within 50 ms | Drained drag, `editor-latency --action set-raw-exposure --parameter ev`, 30 inputs each: 12.5 / 23.7 ms p50 / p95 on the Z6 (load average 15) and 16.2 / 24.5 ms on the X100VI (load average 43–52), against 12.1 / 31.0 ms for Basic's Exposure on the same Z6 (load average 15–17); earlier single trials 10.5 / 14.6 / 15.0 ms on the Z6 / X100VI / Air 2S | **Pass** (every run above the 8.0 load threshold, so the figures are upper bounds) |
| Settled exact histogram p95 below 200 ms after the final input, 24 MP | 70.3 ms p95 (60.1 p50, 30 samples) exposure only; 158.6 ms with a full Basic layer (2 commits) | **Pass** |
| Scratch aggregate at most 64 MiB | 13.46 MiB high-water at 24 MP, 12.82 MiB at 60 MP | **Pass** |
| 24 MP single-image edit working set ≤ 600 MiB CPU-resident | 645.3 MiB peak in the process that commits the full Basic layer, which also retains two full-window capture readbacks; 568.2 MiB in a second process holding the same committed layer with no captures, settling to 408.0 MiB | **Miss by 45 MiB** on the capturing process, **pass** on the same stack without the harness's captures |
| 60 MP peak ≤ 1 GiB process RSS | Final single launches sampled 636.1 MiB on one 60 MP open and 912.2 MiB after sixteen loads; the earlier five-launch run measured 975.0 MiB median on one open and 1316.4 MiB after sixteen loads | **Pass in the final single trials; prior repeated-load distribution missed**. One current launch does not retire the prior multi-run finding |
| Idle CPU < 1% of one core over 30 s | Final single 60 MP idle window: 1.421% over 30.26 s with the default-open Performance section | **Miss** (one window, not a distribution) |
| Geometry input to presented preview p95 < 50 ms once the source preview is ready | not measured for geometry in this round | Open |
| A masked drag costs a person no more than an unmasked one | 24 MP drained drag p50 16.77 / 17.13 ms masked against 16.93 unmasked, 60 MP 17.06 / 17.87 against 17.00 / 17.48, all with a full Basic layer, in both orders at load 2.3–8.1; masked Clarity at 24 MP 17.18 / 17.10 against 17.00 / 17.10 unmasked | **Pass**: every masked run is within 0.4 ms of its unmasked pair at the median, which is inside the spread between two runs of the same condition |

The same two targets at 60 MP, which have no stated threshold and are recorded because the design
asks for the tails: slider-to-presented-frame 125.0 / 141.3 ms and settled histogram 185.4 /
208.8 ms. On the 24 MP crop stack, 150.3 / 170.2 ms and 184.7 / 213.3 ms. The 24 MP crop stack misses
the 100 ms interaction threshold by a wide margin and the largest single contributor is the
`draft.set` round trip, which replans the crop prefix on every set; the 60 MP miss is the
full-resolution render and upload, which is the already-recorded open cost of uploading every preview
at full resolution.

Crop correctness evidence is rendered, not timed: the `crop` and `crop-draft` smoke scenarios record
correlated state, events and pixel checks, and no latency is claimed from them.

The `verify` timing tier reports these provisional targets itself: its summary lists each target
above that `editor-latency` and `measure` can answer, with the measured figure, the sample count, the
exact JSON path it came from and a `pass`, `acceptable` (past the target but inside its acceptable bound), `miss` or `not_measured` verdict, next to the one-minute
load average of the host at the time. A verdict produced from those commands' default sample counts
is a functional check that the targets are still roughly where this table says, not a baseline: a
figure recorded here needs the sample count its own row states, on an otherwise quiet machine. A
summary row or verdict marked `unreliable`, meaning the one-minute load average exceeded 8.0 when its
component started, is never quoted as a baseline or as a pass or a miss.

Core figures exclude desktop scheduling, GPU upload and presentation. Reproduce with
`editor-performance`, `editor-latency` and `measure` as described in
[development](../engineering/development.md). Reports under `artifacts/final-perf-24`,
`artifacts/final-perf-60`, `artifacts/final-latency-24-drag`, `artifacts/final-latency-24-commit`,
`artifacts/final-latency-24-crop`, `artifacts/final-latency-24-crop-commit`,
`artifacts/final-latency-60-drag`, `artifacts/final-latency-60-commit`, `artifacts/final-measure` and
`artifacts/before-measure` retain every sample, the correlated state and the source and binary
hashes; they are local evidence and are not repository assets.

Current macOS `measure` and `editor-latency` runs use background-only bundles to preserve desktop focus, and the editor they launch creates its window invisible. Launch-to-frame timings include copying the executable and creating its temporary bundle; they are background renderer measurements, not foreground activation measurements, and they exclude the cost of placing and compositing a visible window. Nothing in these runs is scanned out, so the presentation figures cover the editor's path to a renderer texture and not what reaching a display would add. Rendering, readback and the state each frame is correlated against are unchanged: the window owns the same Metal surface either way. Reports identify the launch mode. Earlier launch baselines above predate this wrapper and are not directly comparable.

## Current RAW and JPEG measurements

Native Apple M4 Pro, 48 GiB RAM, macOS 26.5.2 (25F84), Metal, 2× scale and a
2880×1800 physical window; files on the internal 2 TB APFS SSD. Release builds,
background-only launches, warm filesystem cache without an OS cache purge. The
RAW application SHA-256 is `ff9eecabdbb3ceaa333885db6bdadb93d86870bfa766a4b95eef13437efa2ea1`;
Cargo.lock SHA-256 is `0c6a739afc2d4830c73059ea2989814950b7607877c59ba945bb89038ea8bd7c`.
The native adapter configuration is recorded in the [backend selection](../research/raw-backend-selection.md).

Thirty complete trials per owner camera passed. Each trial has an isolated catalog,
13 edit/history/view steps, per-step captures and a second-process reopen. The
owner's Z6 is 14-bit lossless NEF; the X100VI is 14-bit uncompressed RAF. All four
public minimum modes also pass one complete trial each; those are functional
checks, not latency distributions. Original hashes, displayed entry/snapshot,
controls, geometry and reopened photo samples are checked together.

Times below are nearest-rank p50 / p95 in milliseconds. Upload readiness is the
application event correlated with the next captured frame, not GPU scanout. Initial
open timings stop at the CPU raster; history and view rows explicitly include
capture readback. Launch-wrapper time and fine-grained stage attribution are not
included in those open figures.

| Measurement (ms, p50 / p95) | Z6 | X100VI |
| --- | --- | --- |
| Initial open → full-resolution CPU raster | 840.0 / 868.2 | 1828.6 / 1887.0 |
| Exposure → upload readiness | 133.3 / 149.5 | 178.8 / 195.8 |
| Red WB gain → upload readiness | 488.5 / 508.4 | 1529.3 / 1625.6 |
| Custom temperature → upload readiness | 482.8 / 493.2 | 1532.1 / 1583.6 |
| Custom tint → upload readiness | 483.9 / 501.2 | 1529.0 / 1575.7 |
| Neutral pick → upload readiness | 483.2 / 501.6 | 1528.9 / 1570.0 |
| Rotate → upload readiness | 118.4 / 126.4 | 234.4 / 243.1 |
| Crop → upload readiness | 98.2 / 119.4 | 128.2 / 141.6 |
| Undo → upload readiness | 116.3 / 124.0 | 231.7 / 240.0 |
| Historical Original → captured frame | 507.5 / 524.8 | 1558.7 / 1600.5 |
| Return current → captured frame | 491.3 / 500.5 | 1615.8 / 1666.4 |
| 100% view → captured frame | 24.9 / 25.5 | 25.2 / 25.8 |
| Edited catalog reopen → CPU raster | 1170.2 / 1222.2 | 3267.8 / 3383.2 |

These history rows predate the development executor, the Rust normalization and the second
development; the current switch times and memory are in [second development](#second-development).
Sampled first-process peak RSS (roughly 50 ms sampling) is 1323 / 1339 MiB p50 / p95
for Z6 and 1975 / 1992 MiB for Fuji. Fuji trial 25 has an unexplained 2436 MiB peak
and a 1038 ms crop update (1007 ms source-to-raster); both tails are retained. Its
pixel, state and reopen checks pass. The capture-heavy workflow cannot isolate
CPU heap, native allocator retention, GPU resources or readback buffers. Separate
screenshot-free live API runs peak at 1583–1647 MiB; the 24-edit run grows only
1.25 MiB after edit three. This does not establish a whole-process bound or prove
absence of leaks. GPU allocations are not measured separately.

Initial development and warm exposure p95 meet their provisional investigation
targets on these files; Fuji memory exceeds the 1536 MiB target. Current WB redevelopment measurements are in
[startup and RAW throughput](#startup-and-raw-throughput). The next resource work is to attribute
the unexplained tail and native/GPU/readback lifetimes, then evaluate bounded
Fit/detail rendering while preserving full-resolution 100% inspection. Budgets
remain provisional; full idle-CPU, cancellation and per-stage measurements remain
open.

The unchanged JPEG core diagnostic was run before and after this integration,
30 samples per recipe and size on the same host with warm filesystem cache.
These exclude desktop scheduling, GPU upload and presentation. Values are p50 /
p95 milliseconds; medians are similar or lower, with mixed tail variation. The
24 MP composed-transform p95 increases by about 1 ms in this run; this is not a
statistical claim of zero regression.

| JPEG core render | 24 MP baseline | 24 MP current | 60 MP baseline | 60 MP current |
| --- | --- | --- | --- | --- |
| One exact transform | 10.43 / 11.22 | 10.45 / 11.42 | 23.27 / 32.26 | 22.01 / 28.84 |
| 200 actions in one orientation layer | 10.33 / 10.81 | 10.52 / 11.80 | 23.45 / 25.16 | 22.14 / 23.35 |
| Same stack plus 10° crop | 32.22 / 36.37 | 32.18 / 34.35 | 73.72 / 98.62 | 71.09 / 77.79 |

Reproduce the JPEG rows with `editor-performance --samples 30` through xtask. The RAW rows were
taken with a 30-trial RAW editor timing tool whose journey is now the `raw-editor` smoke scenario
([development](../engineering/development.md#authentic-raw-evidence)), which records one functional
run's request-to-display times per source; a new RAW distribution is 30 runs of it, not one.
Local reports retain every trial, percentile input, source/binary hash and failure;
private photographs and captures are not repository assets. These observations
qualify the recorded files and host, not other camera modes or platforms.

## Air 2S DNG measurements

The supplied FC3411 uncompressed DNG passes 30 complete background editor trials,
each with the same 13-step editing/history/view journey and second-process reopen.
Original hashes, correction provenance, geometry, displayed state and sampled
photo pixels agree. These are native M4 Pro measurements under the configuration
above, with a warm filesystem on a shared host; host isolation is not claimed.
The release application SHA-256 is `aa24dfa57592c5b3363c34ac2c49b9d28827aa2aa0f77fbf34fce6a2db863e55`; Cargo.lock SHA-256 is `e1f96098ab03786e8976afd4e0ed78b0ed3d4c58cd071c032390552c97b4a600`.

| Measurement (ms) | p50 | p95 | Maximum |
| --- | ---: | ---: | ---: |
| Initial open → full-resolution CPU raster | 1692.6 | 1811.9 | 1812.3 |
| Exposure → upload readiness | 100.2 | 118.5 | 133.2 |
| Red WB gain → upload readiness | 1462.3 | 1536.0 | 1570.6 |
| Custom temperature → upload readiness | 1467.0 | 1530.7 | 1549.9 |
| Custom tint → upload readiness | 1459.7 | 1567.1 | 1597.0 |
| Neutral pick → upload readiness | 1460.0 | 1570.8 | 1659.8 |
| Rotate → upload readiness | 125.7 | 159.3 | 162.0 |
| Crop → upload readiness | 75.7 | 92.3 | 105.2 |
| Undo → upload readiness | 126.1 | 159.4 | 164.6 |
| Historical Original → captured frame | 1484.2 | 1567.7 | 1601.0 |
| Return current → captured frame | 1522.9 | 1583.1 | 1628.1 |
| 100% view → captured frame | 25.6 | 49.9 | 58.2 |
| Edited catalog reopen → CPU raster | 3092.7 | 3412.0 | 3418.2 |

Sampled first-process peak RSS is 1245.3 / 1250.5 MiB p50 / p95,
with a 1258.8 MiB maximum; reopened processes measure
718.6 / 731.3 MiB with a 731.4 MiB maximum. Sampling is
roughly every 50 ms. The optical pass adds one reusable 76.15 MiB active-plane
scratch after native demosaic scratch is released; the allocation ledger is in the
[Air 2S design](../design/air2s-dng.md#allocation-and-performance-review).
The captures, GPU resources and allocator retention are included in observed
process memory, not separated. These values do not establish a process-wide bound,
GPU-memory budget or native Windows/Linux result. The existing Fuji resource
qualification above remains open.

The same JPEG core diagnostic was measured before this change at `3c5def1` and
on this DNG implementation, 30 samples per size/recipe. Values are p50 / p95 ms;
these exclude desktop scheduling, GPU upload and presentation. Both 24 MP current
runs are retained because the first showed higher timings. The repeat's medians
fell below baseline, while crop tails remained higher; 60 MP medians and p95 fell.
The mixed observations do not establish a systematic regression or zero regression
on this shared host. No JPEG raster loop, allocation or desktop message changed.

| JPEG core render | 24 MP before | 24 MP current | 24 MP repeat | 60 MP before | 60 MP current |
| --- | --- | --- | --- | --- | --- |
| One exact transform | 12.30 / 17.37 | 14.88 / 18.02 | 10.83 / 12.76 | 26.11 / 39.25 | 22.11 / 28.61 |
| 200 actions in one orientation layer | 12.09 / 13.71 | 13.88 / 19.21 | 10.51 / 11.49 | 26.48 / 32.65 | 22.01 / 23.56 |
| Same stack plus 10° crop | 37.59 / 43.42 | 44.80 / 51.38 | 32.25 / 52.32 | 84.71 / 91.98 | 74.96 / 88.99 |

The 60 MP current crop has a retained 129.46 ms maximum. Reproduce as above. Local
reports under `artifacts/air2s-editor-30-01/` and `artifacts/air2s-jpeg-*/`
retain every sample, source hash and correlated state; private originals and
captures are excluded from source control. Single-trial Nikon/Fujifilm editor
regressions also pass, separately from the timing distributions.
The host package passes one complete DNG journey with binary SHA-256
`a13a54b0ce2d9c2bf8d7043897988898894931955dfe0a960620623967a2489c`;
its bundled native notices are present and runtime linkage uses no system RAW library.
This is an unsigned macOS development package, not a license audit or platform qualification.

## UI components qualification

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2 (25F84), Metal, internal APFS SSD,
release `--locked`, warm filesystem cache. Desktop captures are 2880 × 1800 at 2×;
the gallery is 2880 × 2000. Baseline commit `86dfa8438b5930ed9736d5bf03fad59ca3c98a8d`,
binary `573cee81b613b5e70351afadb98265899a51aa655f796f5483e4edc0fa585d18`;
qualified application binary `c630aa038041fe73c63358726a9dcd95d20afb0392da14b13b79a50e5cd4e35a`.
The lockfile hash remains `e1f96098ab03786e8976afd4e0ed78b0ed3d4c58cd071c032390552c97b4a600`.
All launches use the background bundle. Matched measurements run sequentially with this task's
compilation and tests stopped; the shared host is not claimed to be quiescent.

The 6000 × 4000 JPEG drag workload uses 30 drained inputs, one release and a coalesced burst.
Input-to-frame ends at the renderer's `Uploaded` callback, not display scanout. The curve is
visible and its module supplies the sampled polyline; its proof effect preserves photo pixels,
so this measures the gesture, draft, preview and upload path, not a future Tone Curve processor.

| Input-to-frame (ms) | p50 | p95 | Maximum | p95 < 100 ms (the threshold then) |
| --- | --- | --- | --- | --- |
| Basic slider before | 75.00 | 91.10 | 92.40 | Pass |
| Basic slider after | 70.66 | 87.75 | 91.82 | Pass |
| Curve point drag | 57.09 | 74.76 | 108.17 | Pass |

The matched slider pair shows no regression. The initial native baseline under concurrent RAW
verification measured 223.68 ms p95 and **missed** the target; it is retained, not substituted into
the matched comparison. An initial sandbox launch could not access macOS services. The initial
curve timing failed strict decimal-value correlation despite ordered rendered inputs; the accepted
run uses exactly representable fractions and keeps the same strict request/frame checks.

Core diagnostics use 30 samples per recipe at 24 and 60 MP and exclude desktop scheduling,
GPU upload and presentation. Values are p50 / p95 ms. Identity rendering remains below 0.01 ms
and shares source pixels; all source hashes remain unchanged.

| Core workload | 24 MP before | 24 MP after | 60 MP before | 60 MP after |
| --- | --- | --- | --- | --- |
| One exact transform | 11.35 / 12.90 | 10.59 / 12.21 | 23.74 / 27.13 | 23.70 / 26.78 |
| 200 actions in one orientation layer | 12.09 / 14.62 | 10.82 / 11.56 | 22.04 / 24.96 | 23.28 / 26.25 |
| Same stack plus 10° crop | 38.21 / 42.93 | 33.45 / 37.10 | 71.87 / 78.17 | 70.66 / 111.85 |
| Exposure on the crop stack | 51.76 / 55.80 | 53.67 / 61.29 | 115.60 / 118.76 | 116.01 / 122.20 |
| Exposure and five Tone fields | 94.26 / 112.15 | 91.08 / 156.10 | 206.06 / 219.86 | 204.94 / 222.84 |
| Vibrance and saturation | 120.02 / 249.29 | 132.22 / 227.33 | 284.31 / 301.14 | 284.65 / 298.20 |
| White balance | 62.41 / 76.42 | 60.39 / 64.29 | 133.80 / 145.05 | 135.06 / 140.08 |
| Histogram reduction | 7.09 / 7.78 | 6.70 / 7.96 | 15.28 / 18.38 | 17.29 / 19.70 |
| Neutral query (25 point samples) | 0.02 / 0.03 | 0.02 / 0.03 | 0.02 / 0.03 | 0.02 / 0.03 |

These core observations are mixed: the 24 MP Tone and 60 MP crop tails increase, while several
other timings fall. No JPEG raster loop changed; these samples do not establish a general
speedup or zero core regression on the shared host. The complete samples, hashes, captures,
state and logs are indexed by `artifacts/ui-components-verification/summary.json`, including
`ui-components-before-matched`, `ui-components-after-slider`, `ui-components-after-curve-final`,
`ui-components-{before-core-24-matched,after-core-24,before-core-60-matched,after-core-60}` and
the retained initial attempts. Gallery and controls smoke cover 63 states on 10 pages and
22 interactions respectively. Existing private RAW, manual visual/fixture-generation and explicit
measurement tests remain skipped; this qualification makes no native Windows/Linux GPU claim.

## Presence, colour mixer and vignette qualification

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, release `--locked`, warm source cache, background bundle launches for every desktop figure; core figures are in-memory synthetic frames rendered by the core alone, warm, with the estimate store of that build warm where a global estimate exists. Every desktop figure ends at the renderer's `Uploaded` callback, not display scanout. All three modules ran the same full-resolution draft path Basic ran: nothing approximate, no extra cache and no timer was added to reach any figure, and each miss below is a finding for the owner's review.

### Core cost of the units

`cargo test --release -- --ignored presence_timing` and the spatial primitive's own timing test, p50 / p95 over 10 runs (the p50 recorded here is the 6th of 10, the tests' upper median before they read the shared nearest-rank `Distribution`, whose p50 is the 5th; the p95 is the 10th either way), one operation over a textured frame, with the process's CPU time over each run as a percentage of one core (p50). Working set is one tile's reserved bytes; concurrency is how many tiles the 256 MiB spatial target allows in flight at once when no other evaluation holds any of it. The Presence rows ran on 23 September 2026 at a one-minute load of 9.6 to 13; the box-blur rows are the primitive's own earlier run, whose test unit ignores the scheduling below.

| Stage | Operation | p50 / p95 ms | CPU | Summed halo | Working set | Concurrency | Budget peak |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 6000 × 4000 | Texture +100 | 241 / 248 | 896% | 8 px | 14.7 MiB | 14 | 205.8 MiB |
| 6000 × 4000 | Clarity +100 | 193 / 198 | 1090% | 199 px | 20.2 MiB | 12 | 242.5 MiB |
| 6000 × 4000 | Dehaze +100 | 128 / 134 | 771% | 67 px | 11.0 MiB | 14 | 154.1 MiB |
| 6000 × 4000 | All three +100 | 942 / 1004 | 1232% | 274 px | 61.3 MiB | 4 | 245.3 MiB |
| 10000 × 6000 | Texture +100 | 629 / 656 | 858% | 14 px | 15.2 MiB | 14 | 213.3 MiB |
| 10000 × 6000 | Clarity +100 | 620 / 644 | 1105% | 327 px | 31.2 MiB | 8 | 249.9 MiB |
| 10000 × 6000 | Dehaze +100 | 355 / 374 | 713% | 107 px | 13.1 MiB | 14 | 183.5 MiB |
| 10000 × 6000 | All three +100 | 3999 / 4116 | 1221% | 448 px | 101.1 MiB | 2 | 202.1 MiB |
| 6000 × 4000 | Host box blur r = 137 (test unit, naive) | 2528 / 2694 | not measured | 137 px | 17.1 MiB | 14 | 240.0 MiB |
| 10000 × 6000 | Host box blur r = 224 (test unit, naive) | 14970 / 15074 | not measured | 224 px | 24.1 MiB | 10 | 240.9 MiB |

The three units together cost more than the sum of the singles, and that is the halo, not a hot loop: with a summed halo of 448 px a 512 px tile reads a 1408 px input region, dehaze fills 1194 px and texture 1166 px of it to deliver 512 px, so over the stage dehaze computes 5.2 times its pixels and texture 4.9 times, and the 101 MiB working set holds a batch to two tiles. Each of those tiles now runs its own passes on the pool (`Parallelism::Pool`, see the [architecture](../design/architecture.md#stage-boundaries)), so the render uses about twelve cores instead of two; on the previous build the same test took 12447 / 12561 ms at 60 MP and 1670 / 1708 ms at 24 MP. Larger tiles would repeat less of the halo, measured below; tiling each unit separately would need an intermediate float frame between units, 687 MiB at 60 MP, over the JPEG frame limit. The vignette's unit alone, single-threaded over 6000 × 4000: 57 ms at amount −50, 505 ms at +50 (the positive branch encodes and decodes each channel), 164 ms at roundness −100.

#### Lazy vignette falloff tables

`Vignette::new` built its two falloff tables — `width + height` entries, one `powf` each on the superellipse branch a negative roundness selects — in the constructor, so every compile paid for them, including `modules/registry/compile.rs`'s stage-only callers (`editor/plan.rs`'s `with_stage_context`: every plan, `draft.set`, a composite step and a query) that need only `describe` or `is_finite` and never read a pixel. The tables are now built lazily, in a `OnceLock` per table, on the first call that actually reads a pixel (`apply_row`); `describe` and `is_finite` no longer touch them, and `is_finite` checks the shape's own coefficients instead, which is exactly as strong because every table entry is a pure function of them.

Native Apple M4 Pro, release `--locked`: 20,000 calls of `Vignette::new` plus one `describe` and one `is_finite` — the owner-thread cost `modules/registry/compile.rs`'s stage-only callers pay per compile — at 60 MP (9504×6336) with roundness −100, so the superellipse branch's tables are the ones measured. A build from before this task carries no comparable instrumentation to compare against — the tables were simply always built — so "before" is reconstructed in this same binary: the lazy build runs exactly the arithmetic the old eager constructor ran (unchanged by this task and still proved bit-identical by the oracle tests), so forcing both tables to build immediately after construction costs exactly what building them inside the constructor did. Measured in one run, reversed (before, after, after, before) so the difference survives the reversal (`compile_cost_per_call_at_60_megapixels`):

| | ns/call (two reversed runs) |
| --- | --- |
| After (lazy, never built — `describe`/`is_finite` only) | 138.1, 136.6 |
| Before (forced eager build, reconstructed) | 103378.6, 105421.5 |

**About 750× faster per stage-only compile call**, from roughly 0.10–0.11 ms down to under a microsecond, consistent across the reversal. At the two to three compiles per slider tick `docs/engineering/performance-rules.md`'s vignette row estimates, this removes on the order of 0.2–0.3 ms of owner-thread time per tick whenever the stack holds a superellipse vignette, against the `draft.set` round-trip p50 of about 0.2–0.3 ms recorded above — the suspected cost this task set out to measure and remove. Renders and `describe`/`is_finite` output are unaffected: the oracle, mirror/flip-symmetry and zero-mask tests above pass unchanged, and a new test (`describe_and_is_finite_never_build_the_tables_and_a_pixel_read_builds_each_once`) proves neither call builds either table and that a pixel read builds each exactly once.

#### Presence exact renders on the generated JPEGs

The core render of the generated 24 MP and 60 MP JPEGs with one Presence layer at +100 in each field it names, warm source and estimates, the 256 MiB target, timed by an uncommitted release probe that calls `render` as `presence_timing` does and reads the process's CPU time around each render; `presence_timing` above is the committed measurement to repeat. The previous build and this one ran four times alternately — previous, this, this, previous — 5 samples per stack and run, at a one-minute load of 5 to 14; every rendered frame of this build had the same SHA-256 as the previous build's for all five stacks at both sizes. p50 of each run:

| Stack | Previous build | This build | Concurrency |
| --- | --- | --- | --- |
| 60 MP, all three | 12758 · 10708 ms, 182 · 192% | 3868 · 3854 ms, 1165 · 1191% | 2 |
| 60 MP, Texture and Clarity | 5605 · 5613 ms, 282 · 280% | 2923 · 2889 ms, 1139 · 1175% | 3 |
| 60 MP, Clarity | 576 · 600 ms | 609 · 636 ms | 8 |
| 24 MP, all three | 1358 · 1414 ms, 371 · 369% | 944 · 929 ms, 1137 · 1166% | 4 |
| 24 MP, Texture and Clarity | 879 · 951 ms | 695 · 682 ms | 5 |

On the previous build the process used at most one core per tile in flight whatever the host's load (182 to 192% for two tiles, at one-minute loads from 5 to 29 across the investigation), which is what the Performance section showed as 135 to 190%; the batches themselves were 93% efficient and the one Dehaze reduction took 6 to 34 ms, so neither was the cause. Where a batch is as wide as the pool (Texture or Dehaze alone) its tiles keep their passes serial, and the two builds agree within 5% in both orders at a target wide enough to make every batch fill the pool. The Clarity row, whose eight-tile batches now spread over the pool, moved by about 5% in either direction across runs. The cost is CPU time: the two runs of this build used 697 and 700 CPU-seconds against 370 and 360 for the previous one over the same renders, because fourteen workers on two tiles' memory-bound passes each run slower and the pool spins between short passes.

Two alternatives were measured and not taken, and a third was adopted later. Raising the spatial target to 4 GiB lets fourteen tiles run at once: all three at 60 MP took 3326 ms at 834% (10 samples, load 10 to 22) but the budget peaked at 1415 MiB, against 202 MiB. Counting only the two plane buffers a tile holds at once would lower the all-three working set from 101.1 to 82.5 MiB, three tiles instead of two. Tiles of 1024 px with pooled passes took 2093 ms for all three at 60 MP and 1616 ms for Texture and Clarity (5 samples, load 8 to 12) with the same bytes on these fixtures and the previous build's CPU time; an operation whose summed halo passes 128 px now runs in them ([tile size by summed halo](#tile-size-by-summed-halo)), and the figures below are the current build's.

#### Tiles written back by row

A spatial tile's output is produced in the frame's own row layout on the worker that ran the tile, the byte frame's RGBA rows quantized and with the input's alpha, and written back with one `copy_from_slice` per row (three per row on the linear planes) instead of a per-pixel copy on the calling thread, and each batch slot reuses its unit scratch instead of allocating and zeroing it per tile. Measured by an uncommitted release probe that renders through `luxforge_core`'s public `render` as `presence_timing` does: one Presence layer at +100 in the fields each row names, warm source and estimates, the 256 MiB target, and the process's CPU time around each render, on the generated JPEGs. It was built against the base `ec132e71` (before) and against this change. Native Apple M4 Pro, release, 28 September 2026, holding the host-wide timing lock; runs back to back and reversed (this, before, before, this) at a one-minute load of 12 to 20, 5 renders per stack and run. Each cell is the p50 of each run, paired in the order the runs went.

| Stack | Summed halo | Before | Rows written by the tile's worker |
| --- | --- | --- | --- |
| 24 MP Texture | 8 px | 156 · 160 ms, 756% · 739% | 141 · 144 ms, 860% · 874% |
| 24 MP Clarity | 199 px | 140 · 141 ms, 980% · 967% | 131 · 129 ms, 1069% · 1101% |
| 24 MP Dehaze | 67 px | 93 · 93 ms, 649% · 654% | 71 · 73 ms, 822% · 829% |
| 24 MP Texture, Clarity | 207 px | 548 · 562 ms, 1070% · 1054% | 536 · 636 ms, 1133% · 963% |
| 24 MP Texture, Dehaze | 75 px | 258 · 273 ms, 965% · 955% | 241 · 325 ms, 1141% · 856% |
| 24 MP Clarity, Dehaze | 266 px | 284 · 308 ms, 1006% · 929% | 252 · 288 ms, 1141% · 1033% |
| 24 MP All three | 274 px | 758 · 776 ms, 982% · 990% | 685 · 788 ms, 1137% · 990% |
| 60 MP Texture | 14 px | 410 · 411 ms, 680% · 679% | 337 · 338 ms, 820% · 827% |
| 60 MP Clarity | 327 px | 501 · 552 ms, 954% · 878% | 447 · 531 ms, 1083% · 926% |
| 60 MP Dehaze | 107 px | 251 · 251 ms, 606% · 616% | 213 · 192 ms, 729% · 808% |
| 60 MP Texture, Clarity | 341 px | 2177 · 2541 ms, 1070% · 919% | 2122 · 2475 ms, 1116% · 964% |
| 60 MP Texture, Dehaze | 121 px | 703 · 824 ms, 937% · 818% | 601 · 701 ms, 1078% · 974% |
| 60 MP Clarity, Dehaze | 434 px | 1196 · 1380 ms, 983% · 856% | 1060 · 1233 ms, 1125% · 975% |
| 60 MP All three | 448 px | 3090 · 3739 ms, 1038% · 847% | 2955 · 3484 ms, 1107% · 972% |

Every frame had the same SHA-256 before and after, for all fourteen stacks, in this pass and in an earlier pass (before, after, after, before) whose load moved between 17 and 73 and whose times are not quoted. A single field, whose batches run many tiles with serial passes, renders 4 to 24% faster with 10 to 20% more of the pool busy: the write the pool used to wait for is gone. Two or three fields, whose batches the target holds to a few tiles with pooled passes, render 3 to 15% faster at 60 MP; at 24 MP, where the write is a smaller share of the render, the pairs lie within the runs' own spread (11% faster to 19% slower). A point sample through the layer is unchanged: it evaluates its tile with an empty scratch slot as before.

#### Tile size by summed halo

`presence_tile_sizes` (`cargo test --release --locked -p luxforge-core --lib presence_tile_sizes -- --ignored --nocapture`, with `LUXFORGE_PRESENCE_SOURCES` naming the sources), native Apple M4 Pro, release `--locked`, 28 September 2026, holding the host-wide timing lock on a host shared with other sessions: the one-minute load was 11 to 36 around the runs. Each row renders one Presence layer at +100 in the fields it names, with a warm source and warm estimates, in 512 px and in 1024 px tiles, alternating which size goes first. A figure is the p50 of 5 renders per size (7 on the synthetic stages and in each row's second figure), with the process's CPU time over the render as a percentage of one core, and the p50 of 9 interior point samples at each size, each equal to the rendered byte. Every row's frame had the same SHA-256 in both sizes. The synthetic stages are the test's textured frames, sized to put halos between the fixtures'. The Tile column is the side `render::limits::spatial_tile` now chooses.

| Stage | Stack | Summed halo | Tile | 512 px: render, CPU · sample | 1024 px: render, CPU · sample | 1024 px against 512 px |
| --- | --- | --- | --- | --- | --- | --- |
| 1440 × 960 (presence fixture) | Texture | 4 px | 512 px | 11 ms, 1013% · 9.6 ms | 12 ms, 998% · 38.8 ms | +9% |
| 6000 × 4000 (24 MP JPEG) | Texture | 8 px | 512 px | 142 ms, 824% · 9.3 ms | 156 ms, 1011% · 37.1 ms | +10% |
| 10000 × 6000 (60 MP JPEG) | Texture | 14 px | 512 px | 332 ms, 837% · 9.4 ms | 380 ms, 1107% · 36.5 ms | +14% |
| 1440 × 960 (presence fixture) | Dehaze | 27 px | 512 px | 5 ms, 1045% · 3.8 ms | 4 ms, 1064% · 15.0 ms | -20% |
| 1440 × 960 (presence fixture) | Texture, Dehaze | 31 px | 512 px | 14 ms, 1066% · 11.4 ms | 14 ms, 1047% · 46.5 ms | +0% |
| 3800 × 2533 (synthetic) | Dehaze | 47 px | 512 px | 30 ms, 878% · 5.0 ms | 26 ms, 994% · 17.4 ms | -13% |
| 3800 × 2533 (synthetic) | Texture, Dehaze | 53 px | 512 px | 118 ms, 1067% · 15.2 ms | 109 ms, 1072% · 58.6 ms | -8% |
| 1440 × 960 (presence fixture) | Clarity | 55 px | 512 px | 6 ms, 1001% · 6.8 ms | 6 ms, 952% · 24.3 ms | +0% |
| 4400 × 2933 (synthetic) | Dehaze | 55 px | 512 px | 38 ms, 848% · 5.2 ms | 37 ms, 1074% · 17.6 ms | -3% |
| 1440 × 960 (presence fixture) | Texture, Clarity | 59 px | 512 px | 19 ms, 1088% · 16.9 ms | 16 ms, 1075% · 59.1 ms | -16% |
| 5200 × 3467 (synthetic) | Dehaze | 59 px | 512 px | 53 ms, 826% · 5.2 ms | 53 ms, 1065% · 18.2 ms | +0% |
| 4400 × 2933 (synthetic) | Texture, Dehaze | 61 px | 512 px | 146 ms, 1160% · 15.4 ms | 140 ms, 1136% · 58.2 ms | -4% |
| 5200 × 3467 (synthetic) | Texture, Dehaze | 65 px | 512 px | 200 ms, 1129% · 15.4 ms | 191 ms, 1133% · 59.8 ms | -5% |
| 6000 × 4000 (24 MP JPEG) | Dehaze | 67 px | 512 px | 70 ms, 809% · 5.4 ms / 73 ms, 832% · 5.5 ms | 69 ms, 977% · 18.7 ms / 66 ms, 1052% · 18.9 ms | -1% / -10% |
| 6000 × 4000 (24 MP JPEG) | Texture, Dehaze | 75 px | 512 px | 227 ms, 1158% · 13.3 ms / 240 ms, 1159% · 13.4 ms | 200 ms, 1133% · 50.3 ms / 224 ms, 1114% · 50.2 ms | -12% / -7% |
| 1440 × 960 (presence fixture) | Clarity, Dehaze | 82 px | 512 px | 9 ms, 1071% · 9.1 ms | 9 ms, 1064% · 32.2 ms | +0% |
| 1440 × 960 (presence fixture) | All three | 86 px | 512 px | 24 ms, 926% · 18.7 ms | 21 ms, 936% · 65.3 ms | -12% |
| 10000 × 6000 (60 MP JPEG) | Dehaze | 107 px | 512 px | 221 ms, 728% · 6.7 ms / 187 ms, 824% · 6.5 ms | 223 ms, 803% · 21.8 ms / 162 ms, 1087% · 20.7 ms | +1% / -13% |
| 12000 × 5333 (synthetic) | Dehaze | 107 px | 512 px | 207 ms, 820% · 7.2 ms | 183 ms, 1049% · 24.3 ms | -12% |
| 10000 × 6000 (60 MP JPEG) | Texture, Dehaze | 121 px | 512 px | 1806 ms, 410% · 15.8 ms / 537 ms, 1216% · 14.9 ms | 732 ms, 815% · 54.6 ms / 492 ms, 1179% · 52.9 ms | -59% / -8% |
| 12000 × 5333 (synthetic) | Texture, Dehaze | 121 px | 512 px | 701 ms, 1153% · 17.2 ms | 640 ms, 1137% · 62.0 ms | -9% |
| 3800 × 2533 (synthetic) | Clarity | 127 px | 512 px | 64 ms, 796% · 11.0 ms | 55 ms, 897% · 34.0 ms | -14% |
| 4400 × 2933 (synthetic) | Clarity | 151 px | 1024 px | 74 ms, 888% · 11.9 ms | 61 ms, 1093% · 34.5 ms | -18% |
| 5200 × 3467 (synthetic) | Clarity | 175 px | 1024 px | 107 ms, 1165% · 12.9 ms | 89 ms, 1119% · 38.3 ms | -17% |
| 6000 × 4000 (24 MP JPEG) | Clarity | 199 px | 1024 px | 125 ms, 1050% · 12.3 ms / 133 ms, 1086% · 12.3 ms | 90 ms, 1017% · 32.9 ms / 97 ms, 1029% · 32.2 ms | -28% / -27% |
| 6000 × 4000 (24 MP JPEG) | Texture, Clarity | 207 px | 1024 px | 561 ms, 1044% · 36.1 ms | 350 ms, 1074% · 95.0 ms | -38% |
| 6000 × 4000 (24 MP JPEG) | Clarity, Dehaze | 266 px | 1024 px | 252 ms, 1120% · 22.3 ms | 157 ms, 1107% · 54.9 ms | -38% |
| 6000 × 4000 (24 MP JPEG) | All three | 274 px | 1024 px | 714 ms, 1079% · 47.2 ms | 546 ms, 972% · 121.0 ms | -24% |
| 10000 × 6000 (60 MP JPEG) | Clarity | 327 px | 1024 px | 697 ms, 663% · 23.1 ms / 402 ms, 1195% · 18.9 ms | 359 ms, 829% · 55.0 ms / 256 ms, 1131% · 42.2 ms | -48% / -36% |
| 12000 × 5333 (synthetic) | Clarity | 327 px | 1024 px | 551 ms, 1103% · 20.1 ms | 360 ms, 1095% · 49.6 ms | -35% |
| 10000 × 6000 (60 MP JPEG) | Texture, Clarity | 341 px | 1024 px | 3030 ms, 768% · 60.5 ms | 1476 ms, 940% · 144.6 ms | -51% |
| 10000 × 6000 (60 MP JPEG) | Clarity, Dehaze | 434 px | 1024 px | 1285 ms, 954% · 38.1 ms | 697 ms, 929% · 79.3 ms | -46% |
| 10000 × 6000 (60 MP JPEG) | All three | 448 px | 1024 px | 3508 ms, 965% · 79.6 ms | 2323 ms, 770% · 171.3 ms | -34% |

The first 512 px figure of the 60 MP Texture and Dehaze row (1806 ms at 410%) was taken while the host was loaded; the second pass gave 537 ms. Up to a summed halo of 128 px the 1024 px tile renders Texture alone 9 to 14% slower, and every other stack as fast or at most 14% faster on a stage of 10 MP or more, while its sample costs three to four times as much. Past 128 px it renders every stack 17 to 51% faster. That is the bound the rule takes ([decisions](../decisions.md#post-consolidation-review)). The frames' identity across the two sizes rests on these hashes rather than on construction, because the box passes' running sums start at each rectangle's edge.

The rule's effect on the whole render, from the release probe of [tiles written back by row](#tiles-written-back-by-row), built at the row write-back (512 px tiles for every stack) and with the rule: runs back to back and reversed (the rule, 512 px, 512 px, the rule) at a one-minute load of 12 to 20, 5 renders per stack and run, and the p50 of 9 interior point samples per run, each equal to the rendered byte. Each cell is the p50 of each run, paired in the order the runs went; every stack's frame had the same SHA-256 in both builds, in this pass and in an earlier one at load 17 to 73.

| Stack | Summed halo | 512 px tiles | By summed halo | Sample, 512 px | Sample, by summed halo | Tile |
| --- | --- | --- | --- | --- | --- | --- |
| 24 MP Texture | 8 px | 141 · 144 ms, 860% · 874% | 396 · 149 ms, 328% · 810% | 9.1 · 9.5 ms | 11.0 · 9.2 ms | 512 px |
| 24 MP Clarity | 199 px | 131 · 129 ms, 1069% · 1101% | 306 · 91 ms, 287% · 1066% | 12.2 · 12.5 ms | 46.0 · 33.1 ms | 1024 px |
| 24 MP Dehaze | 67 px | 71 · 73 ms, 822% · 829% | 76 · 73 ms, 781% · 815% | 5.5 · 5.5 ms | 7.8 · 5.3 ms | 512 px |
| 24 MP Texture, Clarity | 207 px | 536 · 636 ms, 1133% · 963% | 357 · 420 ms, 1138% · 919% | 36.1 · 37.2 ms | 93.9 · 94.6 ms | 1024 px |
| 24 MP Texture, Dehaze | 75 px | 241 · 325 ms, 1141% · 856% | 239 · 333 ms, 1157% · 803% | 13.6 · 13.5 ms | 16.1 · 13.2 ms | 512 px |
| 24 MP Clarity, Dehaze | 266 px | 252 · 288 ms, 1141% · 1033% | 161 · 165 ms, 1126% · 1111% | 22.5 · 22.6 ms | 56.8 · 56.6 ms | 1024 px |
| 24 MP All three | 274 px | 685 · 788 ms, 1137% · 990% | 521 · 616 ms, 1067% · 920% | 46.8 · 46.9 ms | 116.8 · 118.7 ms | 1024 px |
| 60 MP Texture | 14 px | 337 · 338 ms, 820% · 827% | 338 · 422 ms, 830% · 698% | 9.5 · 9.7 ms | 9.5 · 9.6 ms | 512 px |
| 60 MP Clarity | 327 px | 447 · 531 ms, 1083% · 926% | 264 · 279 ms, 1097% · 1088% | 18.8 · 18.9 ms | 42.5 · 43.1 ms | 1024 px |
| 60 MP Dehaze | 107 px | 213 · 192 ms, 729% · 808% | 199 · 201 ms, 767% · 782% | 7.2 · 6.7 ms | 6.7 · 6.7 ms | 512 px |
| 60 MP Texture, Clarity | 341 px | 2122 · 2475 ms, 1116% · 964% | 1535 · 1409 ms, 888% · 971% | 59.1 · 60.1 ms | 154.8 · 134.8 ms | 1024 px |
| 60 MP Texture, Dehaze | 121 px | 601 · 701 ms, 1078% · 974% | 1776 · 671 ms, 372% · 985% | 15.5 · 16.3 ms | 15.6 · 16.9 ms | 512 px |
| 60 MP Clarity, Dehaze | 434 px | 1060 · 1233 ms, 1125% · 975% | 552 · 520 ms, 1138% · 1190% | 38.0 · 41.8 ms | 82.8 · 77.4 ms | 1024 px |
| 60 MP All three | 448 px | 2955 · 3484 ms, 1107% · 972% | 1633 · 1460 ms, 1058% · 1166% | 79.3 · 81.9 ms | 172.7 · 167.5 ms | 1024 px |

Three first-pass figures of the rule's build ran at about 300% CPU while another process held the cores (24 MP Texture and Clarity, 60 MP Texture and Dehaze); their second passes are the comparison, and the 60 MP Texture row's second pass (422 ms at 698%) runs the same 512 px tiles as the build before it. Every stack that now runs in 1024 px tiles renders 22 to 58% faster at about the same share of the pool, so its CPU time falls in proportion: all three fields at 60 MP take 1.5 to 1.6 s instead of 3.0 to 3.5 s. Its point samples cost 1.9 to 3.8 times as much, 33 to 46 ms with Clarity alone at 24 MP and 168 to 173 ms with all three at 60 MP. Texture alone, Dehaze alone and Texture with Dehaze keep 512 px tiles, and their renders and samples are unchanged.

### Desktop slider-to-presented-frame

`editor-latency --mode drag` on the generated 24 MP fixture, 30 drained inputs each, through the new `--action` and `--parameter` selector. The Basic exposure figure in the same harness is 74.8 / 83.4 ms.

| Slider (24 MP, p50 / p95 ms) | Input to presented frame | Settled exact histogram | Peak RSS | Verdict (16 ms target, 32 ms bound) |
| --- | --- | --- | --- | --- |
| Colour mixer, Red hue | 157.1 / 216.1 (min 137.0, max 342.4) | 280.3 / 287.3 | 1357 MiB | **Miss** |
| Vignette, Amount | 121.6 / 135.3 (min 112.0, max 147.9) | 125.0 / 201.4 | 1352 MiB | **Miss** |
| Presence, Clarity | 211.2 / 227.9 | not measured | 1156 MiB | **Miss** |
| Presence, Texture | 230.1 / 244.6 | not measured | 993 MiB | **Miss** |
| Presence, Dehaze | 157.2 / 178.0 | not measured | 1056 MiB | **Miss** |

The mixer's per-pixel cost is the Oklab conversion (three cube roots each way) that Basic's saturation and vibrance units already pay, now paid a second time for a second unit; the vignette's is the extended encode and decode of every channel in its positive branch and the per-pixel mask. Neither exceeds the per-frame cost of the crop resample the earlier rows record, and both stay well under the 1.5 GiB RSS investigation target. The three Presence sliders missed as the design anticipated when each draft rendered the whole 24 MP stage through a tiled neighbourhood operation. These rows predate the instant-preview merge, after which a Presence stack at Fit rendered through the display-bounded proxy, marked approximate; the rows were re-measured below.

### After the instant-preview merge

The same harness on the same fixture after main's instant previews were merged (22 September 2026): at Fit every drafted frame was the display-bounded proxy render, so the input-to-presented figure was the proxy phase, and the exact phase ran behind it for the histogram, the overlays and the 100% view. The Presence frames carry `proxy_approximate: true` in the event log (a spatial layer's neighbourhoods scale with the stage); the mixer and vignette frames are proxy renders that equal the exact recipe at their own scale.

| Slider (24 MP, p50 / p95 ms) | Input to presented proxy frame | Settled exact histogram | Peak RSS | Verdict (16 ms target, 32 ms bound) |
| --- | --- | --- | --- | --- |
| Presence, Clarity | 16.7 / 17.3 | 216.1 / 227.8 | 989 MiB | **Acceptable** |
| Presence, Texture | 22.7 / 26.6 | 266.0 / 271.7 | 912 MiB | **Acceptable** |
| Presence, Dehaze | 16.5 / 17.4 | 159.9 / 164.4 | 908 MiB | **Acceptable** |
| Colour mixer, Red hue | 22.2 / 28.3 | 182.5 / 186.3 | 1267 MiB | **Acceptable** |
| Vignette, Amount | 14.9 / 22.9 | 66.5 / 130.1 | 1261 MiB | **Acceptable** |

The settled-histogram column is the exact phase's cost and stayed where the full-resolution rows above put it, because that is the same 24 MP render; it no longer stood between an input and the frame on screen. Under the owner's target of 2026-09-22 (16 ms p95, acceptable below 32 ms) every slider of the three modules was inside the acceptable bound and none met the 16 ms target; the Presence rows improved by about a factor of ten over their pre-merge rows, and Clarity and Dehaze at 17.3 and 17.4 ms p95 sat just past the target. These drags are now drawn on the GPU ([GPU previews qualified on the M4](#gpu-previews-qualified-on-the-m4)).


Rendered evidence is the `presence`, `mixer` and `vignette` smoke scenarios (15, 8 and 12 correlated frames at Fit and 100% with the module's own controls visible), and the field-patch conformance suite's checks per module through the JSON method table. A reviewer's render of the owner's 14 MP Sapa drone JPEG through the core alone (release, in memory: dehaze 65 ms, clarity 104 ms, texture 127 ms, all three at +50 672 ms) showed Dehaze +60 and +100 lifting the veil and deepening colour plausibly, Clarity +100 adding local contrast without visible halos at fit and at 100%, and Texture +100 sharpening fine detail with the expected crunch; it is a visual check, not a measurement. On a synthetic haze-free flat field Dehaze +100 drives the field toward black, because the dark-channel prior reads a uniform patch darker than the atmosphere as pure veil and the frozen `OMEGA_MAX = 1` removes all of it; the study records this and real photographs, whose windows contain dark pixels, do not show it.

## Brush-heavy recipes across history

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Rust 1.94.0, release `--locked`, warm filesystem cache, catalog on the internal APFS SSD, one process per fixture with its peak memory from `/usr/bin/time -l`. These are recorded measurements of a painting session run once to the per-recipe ceiling, sampled every fifty strokes; the suite gates the curve's shape and square term at 400 strokes (`a_painting_session_grows_with_the_square_of_its_stroke_count`) and the ceiling itself (`a_session_of_long_strokes_ends_at_the_masks_per_recipe_ceiling`), both in `editor/catalog.rs` on the same session harness.

Scope: `luxforge-core`'s own catalog, one stroke per history entry written through the production write path, each stroke captured at 100 positions and decimating to 67–78 stored ones, packed into the densest mask table the declared limits admit. The session is built at the recipe and entry level rather than through `mask.add-stroke`, to isolate the storage write path from command dispatch; the bytes it writes are the bytes the API path will write, because it is the same `insert_entry`. The [stroke-storage table](../design/masking.md#stroke-storage) measures 200 strokes packed 64 to a mask rather than 81, which is the same curve one arrangement less dense: 1.11 MB there against 1.10 MB here.

**Catalog growth is load-independent and is the primary result.** Stored is every entry's JSON plus the content-addressed stroke store; embedded is the same session with each stroke's positions written into its payload instead of its address. The 24 MP and 60 MP fixtures produce identical stored bytes, to the byte, because a stroke is stored in normalized coordinates: the catalog's growth does not depend on the source's pixel dimensions, and only the catalog *file* differs, by a page or two of SQLite allocation.

| Strokes | Stored | Catalog file | Embedded | Factor |
| --- | --- | --- | --- | --- |
| 200 | 1.10 MB | 1.42 MB | 20.3 MB | 18.5× |
| 500 | 5.64 MB | 6.32 MB | 126.5 MB | 22.4× |
| 1000 | 20.9 MB | 22.2 MB | 505.3 MB | 24.1× |
| 1809 (the ceiling) | 66.1 MB | 68.3 MB | 1652.4 MB | 25.0× |

Least squares over the 37 sampled counts gives `S(n) = 19.30·n² + 1623·n + 2511` bytes, identical on both fixtures. Doubling the stroke count multiplies the stored bytes by 3.49 at 250 → 500 and 3.71 at 500 → 1000: **the growth is quadratic and nothing here may be described as linear.** The design predicted about 19 MB at 1000 strokes from the 35-byte reference alone; the measured 20.9 MB is that curve plus each entry's own JSON and the mask and component structure the references hang on, so the design's figure is corrected to the measurement. The design's 105 MB at 2400 strokes is withdrawn: 2400 strokes of this length is not a recipe this build will hold.

**Two ceilings, both measured.** Sixteen masks of 8192 stored positions hold 1809 of these strokes and no more (`a_session_of_long_strokes_ends_at_the_masks_per_recipe_ceiling`). Independently of stroke length, the 256 KiB per-recipe serialized mask bound is reached at 7040 stroke references, which is the most any recipe can hold; past it the write is refused with `resource-limit: recipe masks serialize to 264356 bytes; the limit is 262144 serialized mask bytes per recipe` and the catalog is unchanged, proved by its SHA-256 before and after and by reopening to the same current entry and history length (`a_recipe_over_the_serialized_mask_bound_names_it_and_leaves_the_catalog_as_it_was`). No painting session's snapshot therefore carries more than 256 KiB of mask data.

**The hash-chain variant recorded in the design is not needed and stays unbuilt.** A realistic long retouching session of a few hundred strokes costs one to six megabytes; the largest session of usable strokes that can exist costs 66 MB; and the pathological maximum — 7040 single-position strokes, one entry each, every snapshot at the 256 KiB bound — is at most 1.7 GB, which is a bound and not an open end. Nothing measured here asks for a variant that would cost an O(strokes) walk to rebuild a stroke list on every read.

**Reopen and peak memory are timings on a shared host and are provisional.** The one-minute load average was 20.8 to 25.1 during these runs — far above the 8.0 at which a figure stops being quotable as a baseline — so they are reported as a ratio and an order of magnitude, not as a target. Reopen is `EditorService::open` plus `state` over the 1809-stroke catalog, three consecutive rounds, and the spread within each triple was under 0.2 ms, so the numbers are stable *under that load* even though the load makes their absolute level unreliable.

| Fixture | Reopen (3 rounds) | Load | Peak RSS | Peak footprint |
| --- | --- | --- | --- | --- |
| 24 MP | 14.2 / 14.2 / 14.1 ms | 25.1 | 271 MiB | 266 MiB |
| 60 MP | 21.2 / 21.1 / 21.0 ms | 20.8 | 651 MiB | 647 MiB |

Peak memory is the whole test process, which imports and decodes the fixture. Both processes wrote the identical 66 MB catalog, so the 380 MiB between the two rows is the 36 MP between the two images and nothing else: the history itself is not resident, because entries are written and read one at a time and never held together. Reopen resolves all 1809 stroke references and grows by about 7 ms between the two sizes, which is the source decode and not the store.

### Commit time: writing only fresh strokes

Before this change, `store_strokes` re-wrote every stroke the whole mask table referenced on every commit, an `O(references)` SQL cost per commit and `O(n²)` CPU over a session, almost all of it a no-op `INSERT OR IGNORE` for a stroke an earlier commit already wrote. After, it writes only the references a recipe's stroke table does not already know are durable — known because a fresh hydration marked them, or because an earlier commit built on the same table already wrote them — so a long session's later commits pay only for the strokes each one actually captured.

A binary built before this task carries no commit-time instrumentation to compare against, so the figure below is a controlled A/B within this one binary instead: the same 1809-stroke ceiling session, with the harness's marking of each commit's fresh stroke as stored turned off — this build's own record of the write pattern before this task, since every reference is then treated as needing a write on every commit — and on, the fresh-only write this task lands. Run in reversed order (after, before, before, after) so a difference has to survive the reversal, on a host shared with other sessions.

Scope: the mean write-transaction time (`insert_entry` plus the asset-state update) of the last 50 of the 1809 commits, native Apple M4 Pro, release `--locked`, `luxforge-core`'s own catalog, on the generated 24 MP and 60 MP fixtures. Catalog bytes are unchanged — the table above — because `INSERT OR IGNORE` still guards every write; this changes only how much SQL a commit issues.

| Source | fresh-only (after), ms | every reference (before), ms | one-minute load |
| --- | --- | --- | --- |
| 24 MP | 0.824, 0.815 | 3.114, 3.586 | 11.37 → 10.16 |
| 60 MP | 0.816, 0.993 | 3.022, 3.307 | 7.16 → 6.83 |

The difference survives the reversal on both fixtures: about 3.7–4.4× faster near the ceiling, from roughly 3.0–3.6 ms down to 0.8–1.0 ms of write-transaction time per commit. The saving is expected to widen over a longer session, since the earlier cost was one write per reference the whole mask table carries — which grows with the session — while the fresh-only cost is one write per stroke the commit itself captured, which does not.

### Point samples through a spatial layer

These figures measured the CPU point path stage 4 of [GPU-first](../design/gpu-first.md) deleted: the point-tile cache and the point worker. A sample is now a read by the catalog owner's tile service ([samples from GPU tiles](#samples-from-gpu-tiles)); the figures stay as the baseline TASK-011 measures the GPU's samples against.

Native Apple M4 Pro (14 cores, 48 GiB), release `--locked`, 23 September 2026, on a host shared with other sessions. `render.sample` through the live API of a `develop --background` editor with an isolated catalog and no photograph in its window, at random stage points with the estimate store warm, p50 / p95 over 29 samples after the first. "Before" is the previous build, which built the spatial operation's whole float frame for every RAW sample: the one-minute load moved between 4 and 28 while it was measured, so its p50 range over three runs is given too. "After" ran at load 4 to 5.

| Z6 24 MP · X100VI 40 MP, ms | Before: whole frame | After: one tile |
| --- | --- | --- |
| `render.sample`, Clarity +60 | 292 / 619 · 842 / 1451 (p50 292–568 · 499–842) | 20.8 / 22.1 · 19.5 / 19.9 |
| `render.sample`, Clarity +60 Dehaze +30 | 623 / 1194 · 1495 / 3152 (p50 623–1130 · 1353–1966) | 36.0 / 38.9 · 37.9 / 38.7 |
| Another client's `draft.set` while one samples in a loop, p50 (max); idle 0.3 | 402–578 (762) · 504–647 (1236) | 19.9 (22.3) · 19.2 (22.7) |

On the byte path the same sample on the generated 24 MP JPEG costs 11.5 ms with Clarity +60 and 34.0 ms with Dehaze +30 added (p50 of 15).

#### Off the catalog owner

On the after build the catalog owner only planned a sample through a spatial layer, in `O(layers)`, and its point worker evaluated it and answered the caller, so another client's call no longer waited behind it; the tile service that replaced the point worker keeps a read off the owner the same way. Native Apple M4 Pro, release `--locked`, 25 September 2026: two loopback clients against a background-only, hidden-window editor with an isolated catalog and no photograph in its window, one sampling random stage points in a loop, the other timing 60 `draft.set` calls on an open Basic draft and 60 `asset.state` calls, 30 to 70 ms apart. "Before" is `a82c36e`, "after" this change; each camera ran before, after, then after, before, back to back, and the two orders were run as two passes. **Provisional:** the one-minute load was 18 to 28 during these runs, far above the 8 at which a figure is compared against anything, because other sessions were building on the host; the p50s are consistent across passes, the maxima are not claims.

| Z6 · X100VI, ms, p50 (max) of 60; runs 1 · 2 | Before | After | Idle |
| --- | --- | --- | --- |
| Another client's `draft.set`, Clarity +60 | 18.4 (143) · 14.2 (107) · 11.0 (36) · 14.5 (189) | 0.31 (13) · 0.31 (11) · 0.22 (0.3) · 0.31 (6) | 0.17–0.34 |
| Another client's `draft.set`, Clarity +60 Dehaze +30 | 68.7 (290) · 23.6 (147) · 26.6 (193) · 16.1 (37) | 0.23 (0.7) · 0.32 (4) · 0.24 (0.3) · 0.24 (0.9) | |
| Another client's `asset.state`, Clarity +60 | 26.5 (130) · 19.2 (170) · 0.65 (28) · 9.5 (170) | 0.90 (11) · 0.91 (17) · 0.65 (0.7) · 1.09 (11) | 0.54–1.09 |
| Another client's `asset.state`, Clarity +60 Dehaze +30 | 39.2 (300) · 5.5 (127) · 7.6 (119) · 0.66 (39) | 0.56 (1.1) · 0.90 (4) · 0.66 (0.8) · 0.66 (1.5) | |
| The sampling client's `render.sample` while it runs, Clarity +60 · with Dehaze +30, p50 | 20–33 · 35–118 | 20–38 · 35–53 | |

Samples in the contended window: 101 to 238 per run before, 63 to 189 after (fewer, because the other client's calls no longer lengthen the window). The sample itself costs what it cost on the owner: timed alone, 30 samples in the steadiest runs, 19.7 before against 19.9 ms after on the X100VI with Clarity and 34.7 against 34.8 ms with Dehaze added; the wider ranges above are the host's load, which moved between runs. An `asset.state` before occasionally answered in under a millisecond because it arrived between two samples. An earlier pass with a shorter window (5 to 17 ms apart, load 11 to 16) gave the same picture: `draft.set` p50 10.6 to 31.1 ms before against 0.23 to 0.33 after.

Exactness through the owner was that build's ignored owner test, `a_raw_sample_through_presence_off_the_owner`, deleted with the point worker, run in release with `LUXFORGE_RAW_FIXTURE` set to each private source: on the Z6, X100VI and Air 2S, 21 samples per stack including the far corner, answered by the point worker, each equal to the pixel an exact render of the current entry writes there. Under today's contract the release gate compares samples read from GPU tiles with the reference renderer's frame, within the display limit rather than to the byte, so it reproduces the comparison, not these figures (a selection, so the run reports itself incomplete):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind sample --families presence \
  --sources raw-z6,raw-x100vi,raw-air2s
``` A background evidence run over the Z6 with the after build (`--evidence-script` with Clarity +60, then Clarity +60 with Dehaze +30 through `api` steps, and five `hover` steps including the far corner — the status-bar readout those steps drove was removed on 2026-10-04) showed every readout with no render error, each hover step captured 46 to 69 ms after it was sent.

The method: each commit's release app was copied aside from `target/release/luxforge` and run from a background-only bundle (the plist `develop --background` writes) with `--hidden-window --catalog NEW.sqlite`, driven by two loopback clients read from `NEW.live-session.json`: import the RAW with `catalog.import` and `job.adopt`, commit `edit.set-presence` with `{"clarity": 60}` and then `{"clarity": 60, "dehaze": 30}`, and for each stack open a `set-basic` draft, warm three samples, time 30 samples alone, time 60 `draft.set` and 60 `asset.state` idle, then again while the other client samples random points in a loop. The builds ran before, after, after, before for each camera, with `uptime` recorded around every run. Run on today's editor, the same method times the tile service's reads, not the point worker's.

Exactness on the real files is the ignored core test, run in release with `LUXFORGE_RAW_FIXTURE` set to each private source (`cargo test --release -p luxforge-core --lib a_raw_point_sample_through_presence -- --ignored --nocapture`): 41 samples per stack, spread over the stage and including the far corner, each equal to the byte the linear render writes there. It also times both sides, p50 ms:

| Clarity +60 · with Dehaze +30 | Z6 | X100VI | Air 2S |
| --- | --- | --- | --- |
| Point sample | 20.1 · 35.4 | 18.7 · 36.3 | 15.1 · 27.9 |
| The whole spatial frame the previous sample built | 261 · 550 | 460 · 1131 | 191 · 352 |

The tile's input region is pulled serially. On the shared pool its rows halved an idle sample (Z6 Clarity, 10 against 19 ms) but waited behind a render that held the pool: p50 116 ms against 21 ms serially (15 samples, two alternations each, load 6 to 9), time the catalog owner would spend blocked. The first sample of a stack whose estimates are not yet in the store also reduced the whole stage once, which added 23 to 113 ms across the three files. That reduction was paid only for a unit that declared an estimate key: Clarity and Texture never paid it, and a new Dehaze amount prepared from the stored atmospheric light.

A background evidence run over the Z6 (`--evidence-script` with Clarity +60, then Clarity +60 with Dehaze +30 through `api` steps, and five `hover` steps including the far corner pixel — the status-bar readout those steps drove was removed on 2026-10-04) showed every readout with no render error, each hover step settling within 75 ms of the one before it.

#### In tiles sized by the summed halo

A sample evaluates the tile the render uses, and an operation whose summed halo passes 128 px now runs in 1024 px tiles ([tile size by summed halo](#tile-size-by-summed-halo)). Clarity's halo passes it on all three RAW sources, so its samples evaluate a 1024 px tile there. The ignored RAW test above, which now samples each stack in the production tiling and in 512 px tiles and asserts both against the one render, run in release on 28 September 2026 at a one-minute load of 13 to 15, holding the timing lock (41 samples per stack and tiling, p50 ms; each sample equal to the rendered byte in both tilings):

| Clarity +60 · with Dehaze +30 | Z6 | X100VI | Air 2S |
| --- | --- | --- | --- |
| Point sample, production tiling (1024 px) | 47.1 · 67.5 | 46.2 · 75.2 | 40.3 · 59.5 |
| Point sample, 512 px tiles | 19.0 · 31.2 | 18.0 · 32.3 | 14.5 · 24.5 |
| The spatial frame alone, production tiling | 161 · 245 | 265 · 532 | 125 · 192 |
| The spatial frame alone, 512 px tiles | 223 · 387 | 400 · 731 | 166 · 285 |

On the generated JPEGs a sample through Clarity alone takes 33 to 46 ms at 24 MP and 43 ms at 60 MP in 1024 px tiles, against 12 and 19 ms in 512 px tiles, and through all three fields 117 to 119 and 168 to 173 ms against 47 and 80 to 82 ms ([tile size by summed halo](#tile-size-by-summed-halo)). A stack of Texture, Dehaze or both keeps 512 px tiles and its samples are unchanged: 9 to 10, 6 to 7 and 13 to 16 ms. The point worker answered these samples off the catalog owner, so the longer wait was the sampling client's own and delayed no other client.

#### The stage read by rows

A point query's tile input and every global estimate's reduction read the stage the spatial layer reads through the pipeline's row reader, the one a frame's tiles use: the colour layers before the spatial layer run over whole rows instead of once per pixel, on both pixel domains, and the reduction sums every 16 × 16 block in the one order either way. Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, release `--locked`, 28 September 2026, on a host shared with other sessions: every figure here was taken at a one-minute load of 10 to 29, far above the 8 at which a figure is compared against a budget, so these are relative figures. "Before" is the base `6b876dd4`, "after" this change, each built once and run back to back and reversed (before, after, after, before), each pass holding the host-wide timing lock.

**Identity.** An uncommitted release probe rendered Exposure +0.4, Contrast +20, Vibrance +30 then Dehaze +30 from a cold estimate store and read the Dehaze estimate back, then reduced the same stage from a cold store through a point query. On the generated 24 MP JPEG (byte path), the Z6 and the X100VI (linear path), three rounds each, the frame's SHA-256 and the three estimate values' `f64` bits were the same before and after, and the point query's estimate equalled the frame's in both builds.

**Point samples and a cold reduction.** The same probe, per source and stack, with and without that Basic layer before Presence at Clarity +60 and Dehaze +30: the p50 of 21 samples beside a warm store (20 spread points and the far corner), each equal to the rendered byte, and the p50 of three cold point reductions. Each cell is before (runs 1, 4) → after (runs 2, 3):

| Source | Stack | Sample, no colour before | Sample, behind Basic | Cold point reduction, behind Basic |
| --- | --- | --- | --- | --- |
| 24 MP JPEG | Clarity | 35.1 · 33.6 → 31.7 · 29.5 ms | 349.8 · 344.4 → 104.9 · 102.0 ms | none (no estimate) |
| 24 MP JPEG | Clarity, Dehaze | 65.4 · 56.8 → 67.4 · 52.5 ms | 452.9 · 435.4 → 153.7 · 139.2 ms | 369.1 · 376.9 → 113.1 · 108.7 ms |
| 24 MP JPEG | Dehaze | 5.7 · 5.6 → 5.3 · 5.3 ms | 85.3 · 71.0 → 20.6 · 21.1 ms | 373.8 · 376.2 → 114.3 · 106.0 ms |
| 60 MP JPEG | Clarity | 38.5 · 39.9 → 40.1 · 40.2 ms | 473.4 · 564.3 → 139.4 · 149.9 ms | none (no estimate) |
| 60 MP JPEG | Clarity, Dehaze | 87.4 · 69.9 → 81.0 · 61.5 ms | 616.3 · 618.2 → 219.7 · 167.0 ms | 957.1 · 1702.3 → 331.1 · 258.4 ms |
| 60 MP JPEG | Dehaze | 7.3 · 6.5 → 6.5 · 6.1 ms | 87.5 · 88.9 → 24.4 · 24.7 ms | 1094.7 · 916.0 → 421.3 · 257.5 ms |
| Z6 | Clarity | 57.9 · 48.4 → 46.0 · 47.6 ms | 355.6 · 328.4 → 108.8 · 110.7 ms | none (no estimate) |
| Z6 | Clarity, Dehaze | 77.3 · 68.5 → 81.2 · 67.9 ms | 401.5 · 421.6 → 140.6 · 149.3 ms | 414.9 · 376.4 → 139.5 · 130.3 ms |
| Z6 | Dehaze | 8.5 · 8.9 → 8.1 · 7.6 ms | 66.7 · 65.0 → 22.0 · 21.1 ms | 423.4 · 566.9 → 137.5 · 140.5 ms |
| X100VI | Clarity | 45.6 · 45.4 → 44.1 · 45.0 ms | 408.4 · 326.2 → 111.0 · 212.8 ms | none (no estimate) |
| X100VI | Clarity, Dehaze | 82.2 · 66.9 → 67.7 · 111.3 ms | 495.2 · 408.4 → 158.0 · 226.6 ms | 599.4 · 546.0 → 199.8 · 227.4 ms |
| X100VI | Dehaze | 8.1 · 7.6 → 7.8 · 7.0 ms | 94.4 · 73.1 → 25.2 · 25.6 ms | 785.9 · 556.9 → 219.3 · 340.4 ms |

Behind a colour layer a sample through Clarity costs about a third of what it did, 102 to 150 ms against 326 to 564 ms, and a Dehaze sample a quarter to a third; the cold reduction a Dehaze estimate is prepared from costs 2.4 to 4.5 times less, comparing each row's mean before and after. The X100VI's 212.8 and 340.4 ms were taken at a load of 19.5. With nothing before the spatial layer a pull is a source read and there is no colour to batch: those samples are unchanged, and so are the committed ignored tests' figures, which sample Presence alone. `a_raw_point_sample_through_presence_equals_the_render` gave, in the production tiling, 49.2 · 47.4 → 47.3 · 50.2 ms with Clarity and 72.3 · 68.3 → 68.2 · 70.0 ms with Dehaze added on the Z6, and 56.2 · 46.6 → 50.4 · 47.6 and 78.3 · 75.9 → 90.7 · 77.3 ms on the X100VI (41 samples, each equal to the render); `presence_tile_sizes` gave 42.1 · 33.1 → 32.2 · 34.2 ms for a 24 MP Clarity sample and 82.9 · 78.8 → 78.0 · 131.4 ms for 60 MP Clarity and Dehaze in 1024 px tiles (9 samples). The two to four times that 1024 px tiles added to a Presence-only sample ([in tiles sized by the summed halo](#in-tiles-sized-by-the-summed-halo)) is the unit chain over the larger tile, which reading rows does not touch.

**A RAW Presence exact render.** The same probe's exact render of the Basic-then-Presence stack on each RAW, with a cold store (reducing the stage for Dehaze) and with a warm one, p50 of 3 per run, before (runs 1, 4) and after (runs 2, 3):

| Source | Stack behind Basic | Cold store, before | Cold store, after | Warm store, before | Warm store, after |
| --- | --- | --- | --- | --- | --- |
| Z6 | Dehaze | 908 · 1183 ms | 466 · 473 ms | 430 · 335 ms | 350 · 326 ms |
| Z6 | Clarity, Dehaze | 1061 · 865 ms | 661 · 602 ms | 684 · 550 ms | 485 · 478 ms |
| X100VI | Dehaze | 1598 · 1119 ms | 929 · 1514 ms | 586 · 585 ms | 598 · 1012 ms |
| X100VI | Clarity, Dehaze | 1859 · 1533 ms | 1148 · 1302 ms | 1200 · 949 ms | 906 · 3169 ms |

The difference between a cold and a warm render is the reduction: about 300 to 850 ms on the Z6 before against 115 to 180 ms after. On the X100VI run 3 started at a load of 19.5 and its renders took up to three times as long as run 2's, so only the cold renders of runs 1, 2 and 4 are compared there: with Dehaze 1119 to 1598 ms before against 929 ms after, and with Clarity and Dehaze 1533 to 1859 against 1148 ms. The byte path's frame reads its materialized input and was already cheap; its renders did not change.

**A Basic drag over Presence on RAW.** `editor-latency --source RAW --mode drag --presence --samples 17` at Fit: a Presence layer with all three fields at +100 committed, then a Basic Exposure drag, one launch per run, the four runs back to back at a load of 10.5 to 13.7. The Fit proxy (1716 × 1144 on the X100VI) was rendered whole, so each drag frame reduced the proxy stage for Dehaze, not the exact stage; no window handed it the exact stage's estimate. Seventeen inputs per run, because 30 fail before a frame is measured (below).

| Run | X100VI input to presented frame p50 / p95 | X100VI release to settled histogram (2) | Z6 input to presented frame p50 / p95 | Z6 release to settled histogram (2) |
| --- | --- | --- | --- | --- |
| Before 1 | 45.6 / 52.8 ms | 1353, 1374 ms | 32.4 / 37.0 ms | 775, 804 ms |
| After 1 | 44.9 / 52.1 ms | 1323, 1447 ms | 30.1 / 35.2 ms | 755, 762 ms |
| After 2 | 46.8 / 110.5 ms | 1356, 1388 ms | 31.0 / 43.4 ms | 746, 763 ms |
| Before 2 | 48.0 / 52.9 ms | 1375, 1386 ms | 32.7 / 39.3 ms | 770, 771 ms |

The drag frame is about 2 ms faster at p50 on the Z6 and about 1 ms on the X100VI, in both orders; the settled histogram, which waits for the exact render and its reduction, came 1 to 5% sooner on the Z6 in both orders and did not move beyond the runs' spread on the X100VI. A proxy frame's reduction reads under 2 MP, and an Exposure-only draft's colour is one unit, so there was little per-pixel cost left to remove in this drag. `--basic`, the full Basic layer that would add every colour unit, is refused on RAW, and the default 30-input drag fails on its 19th value (both below), so neither was measured.

**The editor diagnostic.** `editor-performance --source` on the generated 24 MP JPEG (10 samples per recipe), whose stacks hold no spatial layer, ran before, after, after, before at a one-minute load of 9.6 to 15: every row stayed within its runs' spread, for example the full Basic layer at 55.35 · 58.13 → 55.99 · 56.89 ms p50 and its 2880 × 1800 proxy at 32.75 · 34.49 → 34.10 · 33.90 ms.

These drags are 17 inputs because, when they were measured, `editor-latency` generated its 19th exposure value as `0.5700000000000001`, which the desktop's rail refuses; it now generates the decimal the slider sends. One defect in the measuring tool remains: `--basic` commits Temperature and Tint in its Basic layer, which a RAW photo refuses (`on a RAW photo, Temperature is the source development's`), so `--basic` cannot run over a RAW `--source`.

## Preset import parse

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Rust 1.94.0, release `--locked`, in memory, 20 runs each: `cargo test --release --package luxforge-core --lib measure_preset_parse -- --ignored --nocapture`. Each synthetic document is filled to the 1 MiB request limit in the shape that presses one bound, and `inspect_preset` runs detection, parsing, mapping and the report. It reads no file and renders nothing.

| Shape (1 MiB) | p50 / max ms | Outcome |
| --- | --- | --- |
| XMP, one element with every attribute (48,661) | 0.97 / 0.99 | `resource-limit` from the prescan |
| XMP, 1,999 attributes on one element and a curve | 12.57 / 13.75 | 1 mapped, 1,997 unsupported |
| XMP, 199,000 empty elements | 6.51 / 6.73 | 1 mapped, 1 unsupported |
| XMP, 128 namespaces in scope and a 43,000-point curve | 7.06 / 7.55 | 1 mapped, 1 unsupported |
| XMP, 1,748 nested structures of 40 fields | 12.89 / 13.14 | 1 mapped, 1 unsupported |
| Template, 59,482 unrecognised settings | 11.82 / 12.12 | 1 mapped, 59,482 unsupported |
| Template, one flat curve just under 100,000 values | 5.90 / 6.15 | 1 mapped, 1 unsupported |

The XML parser checks each element's attributes against each other, so its cost grows with the square of an element's attribute count. Before the prescan bounded that work to 2,000,000 comparisons, the first shape took 4.1 s p50 and 7.4 s max. The prescan also bounds nesting, which the parser descends recursively, and namespace declarations, which it scans for every prefix.

## Module capabilities qualification

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, release `--locked`, on 23 September 2026, on a host shared with other sessions: the one-minute load average was 3 to 11 during these runs and is given per row. "Before" is `ca8eaef`, the last commit without the framework; "after" is `5f77f58`. No figure here is a p95 claim beyond its stated sample count.

### The framework's own costs

`cargo test --release --locked -p luxforge-core --lib capability_timing -- --ignored --nocapture`, isolated directories, an in-memory secret store and the loopback proof endpoint; load 10.9 at the start. The p50s recorded here are the upper median (the 16th of 30, the 101st of 200) the test took before it read the shared nearest-rank `Distribution`, whose p50 is the 15th and the 100th; the p95s are unchanged.

| Measurement | p50 / p95 | Samples |
| --- | --- | --- |
| Registration, the eight built-ins | 0.020 / 0.022 ms | 200 |
| Registration, built-ins and the proof module | 0.031 / 0.035 ms | 200 |
| `module.status` owner round trip | 0.013 / 0.023 ms | 30 |
| `module.settings.read` owner round trip | 0.009 / 0.012 ms | 30 |
| A whole `task.generate-proof-tint`: request to `ready`, including the 64 samples, the file read, the loopback request, the artifact publish and its row | 14.9 / 15.8 ms | 30 |
| … of which publishing one 12-byte artifact (synced, renamed) | 9.1 / 9.9 ms | 30 |
| Cancel a task stalled inside its request to `cancelled` (the cancel shuts the request's socket down; measured 2026-09-27 on the [`ureq` transport](#transport-on-ureqs-agent), load 35 to 39) | 0.28 / 0.89 ms | 10 |
| Installed proof resource on disk, `installed.json` included; staging left behind | 448 bytes; none | 1 |

The two request rows were measured through the real transport against the loopback proof endpoint. Since the transport moved to `luxforge-net`, `capability_timing` sends through the core's in-memory transport, which checks a cancel every millisecond, so a new run of those two rows measures the host around the request, not the network path; the real transport's cancel latency is the one in the [module capabilities design](../design/module-capabilities.md#transport).

Discovery, registration and catalog reopen start no worker thread and create no directory: `schema.list` and `module.list` against a fresh data root leave it empty, and the owner tests assert that no lane has started and the secret store saw no call. The two lanes block on their channels while idle.

### Editor before and after

`measure` (5 launches per workload, background bundle) and `editor-performance` (24 MP, 30 samples), before and after, run back to back.

| Measurement | Before | After | Load |
| --- | --- | --- | --- |
| Launch to first frame, empty (p50 / p95) | 963 / 1084 ms | 1012 / 1072 ms | 3.2 |
| … of which until the process runs (median of the empty and 24 MP launches) | 419 ms | 465 ms | 3.2 |
| … of which process start to first frame (median) | 597 ms | 589 ms | 3.2 |
| Launch to first frame, 24 MP / 60 MP (p50) | 1022 / 1136 ms | 1081 / 1191 ms | 3.2 |
| Sampled peak RSS, empty / 24 MP / 60 MP (median) | 158 / 419 / 850 MiB | 139 / 392 / 853 MiB | 3.2 |
| Idle CPU over 30 s | 0.53% of one core | 0.50% of one core | 3.2 |
| Executable size | 20.4 MB | 24.8 MB | — |

The whole launch difference is in the part before the process runs, which for these background launches includes copying the executable into a temporary bundle: the executable is 4.3 MB larger (rustls, ring and the platform verifier) and now links Security.framework. Process start to first frame is unchanged. The core `editor-performance` rows (transforms, crop, Basic colour stacks, proxy renders, the histogram and the picker) moved by a few percent in either direction depending on which build ran second while the load rose from 5 to 11.6, so they show no difference attributable to the framework; the render path gained only an early return for stacks without artifacts. The full `verify` tier on `5f77f58` passed every provisional target except the empty-shell launch p95 (1012 ms against 1 s), which the baseline also misses under the same bundle copy.

### Transport on `ureq`'s agent

Release builds (`cargo xtask build --release`) of `cdd5667`, whose transport framed HTTP/1.1 with `ureq-proto` in its own read loop, and of the change moving it onto `ureq`'s agent and pinning `idna_adapter` to 1.0.0, in one worktree on the M4, 2026-09-27. Size only; the cancel row above is the one timing this change moved.

| Measurement | Before | After |
| --- | --- | --- |
| Executable size | 30,106,048 bytes (30.1 MB) | 30,102,240 bytes (30.1 MB), 3.7 KiB smaller |
| `Cargo.lock` packages | 509 | 488: `ureq` and `utf8-zero` become normal dependencies, and the 21 packages of the ICU4X normalizer behind `idna_adapter` 1.2.2 go |

The 24.8 MB above is the executable at `5f77f58`; the features added since account for the rest of the difference.

### The transport outside the core

`cargo test -p luxforge-core --no-run --locked` on the M4 Pro (14 cores), Rust 1.94.0, dev profile, 2026-09-28, before (`494beb2b`, the transport and Keychain store in the core) and after (the change moving them to `luxforge-net`), one run each in the order before, after, after, before, in one worktree on a host shared with other sessions: the one-minute load average was 13 to 27 across these runs, so the wall times vary by more than any difference between the builds, and CPU time (user, from `/usr/bin/time`) is the steadier figure. "Cold" is a fresh target directory, every dependency built; "clean core" is `cargo clean -p luxforge-core` over built dependencies, which rebuilds the core and `luxforge-testkit`.

| Measurement | Before | After |
| --- | --- | --- |
| Packages compiled, cold | 105 | 86: `rustls`, its pki types and webpki, `ring`, the platform verifier, `security-framework` and its `-sys`, `ureq`, `ureq-proto`, `http`, `httparse`, `base64`, `utf8-zero`, `log`, `once_cell`, `subtle`, `untrusted`, `getrandom` 0.2 and `bytes` go |
| Cold, wall / user CPU | 86.0 s / 436 s; 140.3 s / 483 s | 97.3 s / 402 s; 85.3 s / 401 s |
| Clean core, wall / user CPU | 56.8 s / 295 s; 54.4 s / 307 s | 68.1 s / 314 s; 56.4 s / 295 s |

A new worktree's first build of the core's tests compiles 19 fewer packages and spends about 35 to 80 s less CPU, 8 to 17 percent. Rebuilding the core alone shows no difference outside the noise: the transport was a small part of the core's own code, and the dependencies it no longer links were already built. The core's normal dependency tree (`cargo tree -p luxforge-core -e normal`) went from 80 packages to 61.

## Performance section, activity board and resource counters

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Rust 1.94.0, release builds.
The underlying counter measurements below are from 2026-09-23; the `resources_cost` and
GPU-walk p50s are the upper median (the 501st of 1000 reads, the 101st of 200 walks)
those probes took before they read the shared nearest-rank `Distribution`, whose p50 is the
500th and the 100th; their p95s are unchanged. Counter contracts and one-second resolution are
unchanged by the 2026-10-02 panel polish.

| Measurement | Result | Scope |
| --- | --- | --- |
| `resources.read` in the desktop, cache warm (`declare_gpu_presenter` called, this process's GPU clients cached) | p50 4.2 µs, p95 7.1 µs; 5.2 / 7.8 µs with JSON encoding | 1000 reads, `cargo test --release --locked -p luxforge-core --test cancellation -- --ignored --nocapture resources_cost`, load 22 to 30 |
| One full walk of the GPU registry (82 to 84 user clients) | p50 0.31 to 0.35 ms, p95 0.39 to 1.1 ms | 200 walks, `cargo test --release --locked -p luxforge-process --test cost -- --ignored --nocapture`; taken at most every 10 s |
| First read after `declare_gpu_presenter` | 0.6 ms when the Metal device already exists (the desktop), 37.5 ms in a process that has none | One read each |
| `activity.list` / `resources.read` / `session.state` round trip through the headless `luxforge-json` owner, stdio and JSON included | p50 14.8 / 19.5 / 18.8 µs, p95 23.0 / 28.1 / 30.9 µs | 2000 requests each after 50 warm-up, load 12 to 18 |
| Activity `begin` + `finish`, uncontended | p50 83 ns, p95 84 to 125 ns | 100,000 iterations; the timer resolves 42 ns |

The section starts open on first use, saves the last disclosure choice outside the catalog through
`preferences.read` and `preferences.set`, and samples only while expanded and visible. Collapsed
or hidden it creates no timer and makes no reads. A listed running job with an ID offers Cancel
through `job.cancel`; the native `capabilities` check presses that row's actual message and proves
the matching job cancelled without changing the accepted edit. Native `performance` frames
independently reconstruct the counters, sparklines and rows and prove collapsed sampling stays
asleep. Idle Performance messages refresh only their model; active work and evidence keep the
full derivation path. Unchanged GPU tiles avoid layout allocation and uniform writes.

Ordinary-session idle measurements use baseline binary SHA-256
`72bdcfb472e5bfb1c277a362ba4f4dd935efa3dc818b051115ecd71eddffddb4` and panel-polish binary
`066e6f1123ae060d6cd1738d810cb30ec49b90cbea4e1ee6d34d5d140df8c4a7`, with
before/after/after/before order on 2026-10-02: `editor-latency --source fixtures/generated/24mp.jpg --mask --samples 30
--idle`. Each run reopens its committed masked Basic stack without evidence mode, settles one
second, then measures process CPU-time delta for 30 seconds. Performance is expanded; its
one-second sample and whole-window redraw are included. Native GPU accounting comes from the
preceding gesture's captured state, not an idle-process capture. No local build or test overlaps.

| Run | CPU, % of one core | Duration, s | Sampled peak RSS, MiB | Idle start → end one-minute load |
| --- | --- | --- | --- | --- |
| Before 1 | 0.953 | 30.425 | 380.8 | 13.05 → 16.62 |
| After 2 | 0.827 | 30.239 | 468.3 | 7.76 → 6.81 |
| After 3 | 1.190 | 30.264 | 374.5 | 6.58 → 5.86 |
| Before 4 | 0.894 | 30.217 | 366.3 | 5.63 → 12.90 |

These windows do **not** establish an aggregate idle CPU improvement. Both baseline windows
encounter host load above the quiet threshold of 8; both optimized windows stay below it, but
one still exceeds the provisional 1% idle target. RSS includes allocator retention and GPU
resources and sets no memory-reduction claim here. There is no paired collapsed window, so
these numbers do not isolate the section's extra CPU. The remaining toolkit view, layout and
whole-window redraw cost is still open. Raw reports are in
`artifacts/mask-perf-20261002/idle-*/resources.json` and `comparison.json`.

The isolated release probe `app::performance::tests::performance_model_work` compares the former
full-workspace derivation route with the scoped refresh on the same final code and imported 24 MP
photo. Each route receives a changed resource sample, with a full 61-sample history; order alternates
within each pair. After 200 warm-up pairs, 1000 samples per route give full derivation p50/p95
60.38/64.21 µs and scoped refresh 1.46/1.58 µs: about 98% less model work. Pixel workers are stopped
before timing; import, decoding, owner calls, widgets, layout and GPU work are excluded. This is
a component comparison, not an end-to-end redraw or ordinary-session CPU reduction. The shared
host's one-minute load is 8.64. Every sample is retained in `model-work.json` beside the idle reports.

## Isolated rendering kernels

The Presence scalar accessor is inlined, RAW terminal conversion reuses the existing sRGB
code-boundary table with the original forward conversion within `1e-12` linear of a boundary,
and Basic's skin-hue weighting uses its bounded atan2 input directly without periodic normalization.
These changes preserve tile geometry, filter arithmetic, scratch targets, scheduling and image
semantics. The boundary fallback retains the previous forward rounding in the native exactness
checks; cross-platform numerical qualification remains open.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0 release, generated 6000 × 4000
and 10000 × 6000 inputs. Each isolated candidate and retained baseline ran in before/after/after/before order,
15 samples per leg, 30 per variant and size, with one warm-up per process. These are complete
core renders, including the output allocation, from already-prepared data; JPEG decoding,
RAW decoding/demosaicing, proxy creation, histogram reduction and desktop presentation are
outside the timed region. The RAW workload converts the generated JPEG into planar linear
floats before timing and applies source exposure +0.7 EV; it is not an authentic RAW development
latency measurement. The baseline source is `22c4e90`.

| Workload | Baseline p50 / p95 | Optimized p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| 24 MP, Texture +100, Clarity +100, Dehaze +100 | 815.8 / 877.9 ms | 664.1 / 791.1 ms | 151.7 ms, 18.6% |
| 60 MP, Texture +100, Clarity +100, Dehaze +100 | 3688.8 / 3791.9 ms | 2877.7 / 2927.0 ms | 811.2 ms, 22.0% |
| 24 MP, prepared linear image, source exposure +0.7 EV | 68.0 / 70.9 ms | 57.7 / 62.4 ms | 10.3 ms, 15.1% |
| 60 MP, prepared linear image, source exposure +0.7 EV | 173.8 / 185.4 ms | 146.2 / 152.2 ms | 27.6 ms, 15.9% |
| 24 MP, full Basic | 121.7 / 143.6 ms | 119.0 / 129.7 ms | 2.7 ms, 2.2% |
| 60 MP, full Basic | 304.7 / 315.6 ms | 296.0 / 318.6 ms | 8.6 ms, 2.8% |
| 24 MP, Vibrance +50 / Saturation +20 | 87.4 / 93.9 ms | 84.6 / 88.2 ms | 2.8 ms, 3.2% |
| 60 MP, Vibrance +50 / Saturation +20 | 222.9 / 234.1 ms | 214.7 / 237.3 ms | 8.1 ms, 3.7% |
| 2700 × 1800 proxy from 24 MP, full Basic | 26.1 / 27.4 ms | 25.3 / 29.3 ms | 0.9 ms, 3.3% |

Every Presence and RAW candidate leg beats both baseline legs for its workload; Basic's median
improves in each paired order, including the proxy. Basic's 60 MP and proxy p95 values are slightly
worse, so only a median improvement is established there. Full Basic uses all ten non-neutral
fields of `editor-performance`'s full Basic layer, without its geometry tail in these kernel runs.
Agents' builds and tests were paused during timing. One-minute load was 5.0–8.0 (strictly below
8 at each leg's start) for Basic, 7.0–9.4 for RAW and 12.2–15.2 for Presence, including
the benchmark itself; the Presence results and some RAW legs exceed the harness's 8.0 load
threshold. They establish a relative improvement on this live host, not a new absolute latency
budget or a passed responsiveness target. Presence's 60 MP median CPU time per leg falls from
43.6 CPU-seconds to 33.3–33.4 CPU-seconds, so the wall-time gain also reduces total CPU work.

Complete before/after RGBA buffers match byte for byte for Presence at both sizes, prepared
linear exposure at both sizes, a full Basic layer over a 24 MP linear image, and Basic/vibrance
at both JPEG sizes plus the full Basic proxy. Independent
filter, tile, masked sample/render and quantization-boundary references supplement these
photo-sized comparisons. The unchanged 512 px tile still incurs the halo amplification and
point-sampling cost described above. The current [Markesteijn row executor](../design/native-demosaic-parallelism.md) preserves tile
equations and bounds scratch without adding another thread pool; its measurements are
separate in [startup and RAW throughput](#startup-and-raw-throughput).

The combined source also passes release `editor-performance` against the retained baseline on
both source sizes, again 15 samples per leg in ABBA order, 30 per variant. This official diagnostic
includes its composed transforms and 10° crop, so these values are separate from the bare kernel
rows above. Full Basic at 24 MP is essentially flat (0.4% median reduction); at 60 MP it saves
3.5%. Vibrance/Saturation saves 3.3% and 2.0%. Untouched median rows vary by up to about 3%, so
smaller shifts are not attributed to an optimization. All original-source hash checks pass.
One-minute load at the start of a leg ranges from 4.99 to 15.87; these are live-host comparisons,
not new absolute budgets.

| Integrated `editor-performance` workload | Baseline p50 / p95 | Integrated p50 / p95 |
| --- | --- | --- |
| 24 MP source, Full Basic + geometry | 83.0 / 89.7 ms | 82.7 / 88.6 ms |
| 24 MP source, Vibrance/Saturation + geometry | 68.1 / 73.7 ms | 65.8 / 69.4 ms |
| 24 MP source, Full Basic proxy + geometry | 51.0 / 53.2 ms | 49.6 / 52.4 ms |
| 60 MP source, Full Basic + geometry | 202.2 / 277.9 ms | 195.1 / 202.8 ms |
| 60 MP source, Vibrance/Saturation + geometry | 160.8 / 192.2 ms | 157.7 / 161.3 ms |
| 60 MP source, Full Basic proxy + geometry | 53.9 / 58.7 ms | 53.4 / 58.3 ms |

Integrated `verify --tier full --manifest ...` passes all 38 components: workspace/API checks,
30 rendered scenarios, RAW references, three editor/reopen trials each on the supplied Nikon,
Fujifilm and DJI originals, and the timing journeys. Its default timing runs started at load
9.86–11.20, so their absolute target verdicts are **unreliable**, not passed. The existing native
Presence sample/render test also passes explicitly on all three originals, for 41 points including
the far corner through each of Clarity and Clarity plus Dehaze. Those test times are not a new
latency distribution.

The local evidence is under `artifacts/performance-first-wave/`; the kernels' contract and
performance review checklist are [below](#the-isolated-kernels-contract-and-review).

### The isolated kernels' contract and review

These CPU kernels preserve photo output, editing semantics, recipes, API shapes, scheduling and
memory targets.

**Presence.** The filters' plane accessor (`Plane::get`) carries an ordinary inline hint. The
release compiler can fold its coordinate clamps and address arithmetic into the callers; the M4
comparison found no remaining out-of-line calls to this accessor. Edge clamping, pixel arithmetic,
f64 filter accumulation, the tiling, parallelism and scratch bounds are unchanged by it, and it
adds no specialized assembly or architecture-specific path.

**RAW terminal conversion.** Finite values are clamped at the terminal boundary and quantized
through the static sRGB code thresholds. Within `1e-12` linear of either adjacent code boundary,
the original forward power/round evaluation is kept: forward and inverse floating-point transfer
functions can disagree on the final byte at a boundary. Non-finite rejection and the extended
linear domain before terminal conversion are unchanged; JPEG quantization keeps its own contract.
The guard is conservatively tested on the M4, not a formal cross-platform error bound for `powf`.

**Basic hue weighting.** The private skin-hue response accepts the bounded angle from atan2.
Subtracting its 55° centre gives `[-235°, 125°]`. The only part that would wrap lands in
`[125°, 180°]`, outside the ±35° active band either way, so the response uses the delta directly,
with no general normalizer or remainder, and the band test and cosine arithmetic unchanged.

**Exactness.** Independent filter references, serial/pool comparisons, tiles, masks and
sample/render parity cover Presence. RAW tests compare the original forward terminal conversion
at all 255 code boundaries and 128 f64 neighbours on either side, at the guard edges, through a
dense sweep and deterministic float bit patterns, and on signed, headroom and non-finite inputs.
An independent periodic oracle checks the hue response's boundaries and a million bounded angles.
Complete 24/60 MP before/after output buffers provide a separate photo-sized check. Measured
kernel savings are not presented as desktop latency or demosaic speedups, and native
Windows/Linux numerical and desktop qualification remain separate from the M4 evidence.

| Review question | Answer for these kernels |
| --- | --- |
| Original reads, hashes and decodes | No new request paths. Preparation continues through the signature-verified source cache. |
| Frame allocations and sharing | No new buffers. Existing output allocations, identity sharing and spatial/colour targets remain in force. The RAW quantizer reuses the existing static threshold table. |
| Point queries, validation and no-op checks | No new rasterization. Spatial sampling keeps its declared one-tile exception and exact render parity. |
| Catalog owner work | No work moves to the owner. |
| Desktop messages and uploads | No changes to state/history refresh, preview jobs or uploads. |
| Timers, polls and subscriptions | None added or changed. |
| Photo-sized cost | Release before/after evidence is recorded above, with kernel diagnostics distinguished from `editor-performance` and native desktop journeys. |
| Exactness and sharing tests | Presence keeps the independent filter references, serial/pool, tile, mask and sample/render checks. RAW tests compare the original forward conversion at every code boundary and across finite/non-finite inputs. Hue tests compare the periodic formula at boundaries and through a million bounded angles. Full photo-buffer comparisons supplement these references; existing identity-sharing tests still apply. |

### Straightened-crop resample

The byte resample's bilinear sample rounds each channel forward, `round(255 · encode(v))`, through
the same guarded threshold search as the RAW terminal (`srgb::Quantizer::rounded`): the code-boundary
index answers every value more than `1e-12` linear from both thresholds around its code, and the
forward transfer function answers the rest. A 10° crop of the 24 MP fixture outputs 3695 × 2077, so
this removes about 23 million `powf` calls from the exact render. The colour row loops, the byte
proxy build, the byte spatial entry's read and the RAW terminal rows take the decode table and the
quantizer once per pass instead of dereferencing a lazy static per pixel, the linear point path
reads its source through the view resolved once at construction, and the finiteness scan after each
colour unit no longer short-circuits, so it vectorises.

Byte identity: the boundary tests drive `bilinear` itself across every code threshold, with
horizontal and two-row blends of three corner pairs per threshold stepped ULP by ULP through and
around the guard band, plus 200,000 random frames and taps; the quantizer without its guard fails
them. Full-frame SHA-256 of the exact render, the exact render with a full Basic layer, the display
proxy source and both proxy renders, and 2048 point samples each, at 3°, 10°, −7.5° and 45°
`crop-fit`, match between a release build of `7759f5cb` and this change for the generated 24 MP and
60 MP JPEGs, the photographic `will-sapa-drone.jpg`, the Z6 NEF and the X100VI RAF (140 hashes).

`editor-performance --source fixtures/generated/24mp.jpg --samples 30`, release `--locked`, native
M4 Pro, warm cache, two ABBA rounds: A is the retained `ec132e71` baseline build, whose resample,
colour loops and linear point path are those of `7759f5cb`; B is this change. The host was heavily
shared: the one-minute load at the start of each leg was 10.7, 14.3, 15.2 and 29.3 in the first
round and 12.0, 15.2, 15.6 and 17.1 in the second. p50 of each leg, in run order:

| 24 MP, p50 ms (A, B, B, A) | Round 1 | Round 2 |
| --- | --- | --- |
| 200 transforms and a 10° `crop-fit` | 30.5, 22.0, 29.7, 52.5 | 35.7, 18.9, 22.9, 42.4 |
| The same, less the 200-transform row (the crop's own cost) | 24.0, 12.9, 18.1, 41.5 | 28.6, 10.5, 15.5, 33.9 |
| Proxy frame of the crop stack (2879 × 1618) | 21.7, 11.9, 12.8, 24.5 | 22.5, 10.5, 11.7, 27.9 |
| Proxy frame of the crop stack with a full Basic layer | 69.1, 47.9, 45.6, 51.9 | 52.7, 42.8, 45.0, 66.2 |

Every B leg is below both A legs of its round in all four rows. The straightened crop's own cost
over the unrotated stack falls from 24 to 42 ms to 11 to 18 ms at the median, and the proxy frame
of the crop stack roughly halves, from 22 to 28 ms to 11 to 13 ms. At this load these are relative
results, not new absolute budgets. The proxy build row is not compared: the baseline predates the
banded proxy build.

## Source preparation and exact rendering

The source worker develops a cold known RAW directly at the validated requested white balance.
Mosaic normalization and the output scale run in 16-row jobs on the development executor above one
megapixel, for Bayer and X-Trans alike. DNG optical corrections run disjoint 16-row jobs on the same executor.
Markesteijn and RCD use the bounded tile jobs described in [startup and RAW throughput](#startup-and-raw-throughput).
Texture and Clarity skip unused global reductions; Dehaze reuses its atmosphere across strength edits.
Its cache distinguishes upstream masks and sampling, source development/view, exposure and approximate
white balance. Terminal encoding indexes the exact code boundaries, retaining the RAW boundary guard;
source-only RAW rendering resolves the planar view once per row. Clipping reduces display grids into
one output allocation, with at most 4 MiB of fixed partial scratch for tiny grids that would otherwise
underfill the pool. The [contract and review checklist](#source-preparation-and-rendering-contract-and-review)
follow the method below.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, release with locked pins,
25 September 2026. Baseline production source `3890a5b`; integrated source `75cca64`.
All agent builds/tests were paused during timing. Each reported distribution combines 15 observations
per leg in before/after/after/before order: 30 per variant, with no tails removed. Kernel leg-start
one-minute load was 10.2–16.9, including the benchmarks themselves, above the harness's 8.0 threshold.
These are relative live-host comparisons, not passed absolute latency budgets. Raw observations,
commands, hashes and load are retained locally in `artifacts/performance-second-wave/`, and the
research evidence, against `3890a5b`, in `artifacts/performance-next/`.

### Source preparation and rendering: contract and review

These changes keep the existing image and editing contracts and introduce no approximation or GPU
behaviour. Originals stay immutable, nothing moves pixel work onto the owner or UI thread, and no
user-facing API, recipe or catalog shape changes: existing commands gain the performance through the
same implementation. One active preparation/preview lane and the existing resource limits remain,
and new parallel work shares the process pool, measured under contention as well as in isolation.
The source design contracts are [initial RAW](../design/initial-raw.md),
[Air 2S](../design/air2s-dng.md), [Presence](../design/presence-mixer-vignette.md) and
[instant previews](../design/instant-preview.md).

1. A cold known RAW develops once, at the validated requested white balance, instead of as-shot
   followed by the saved gains; a new import keeps as-shot. Fingerprints, source interpretation,
   historical entry identity, concurrent changes and generation adoption stay authoritative.
2. Texture and Clarity declare that they need no global estimate. Dehaze's estimate identity
   excludes only its own strength: source development and view, upstream recipe and masks,
   sampling mode, stage, exposure and white-balance identities invalidate it. Provider defaults stay
   safe and caches bounded.
3. DNG stage-3 optical corrections run disjoint rows on the development executor above a measured
   threshold, keeping stage and channel order, f64 operations and tap summation, finite failures,
   cancellation and the single active-area scratch plane, with no private pool or additional
   full-frame allocation.
4. Terminal quantization indexes the exact code thresholds through a small static coarse table,
   keeping tie and clamp behaviour and the RAW canonical boundary guard.
5. Clipping reduction partitions disjoint output rows into one final grid for display-sized grids.
   Tiny grids with too few output rows use at most 64 fixed partial grids of at most 64 KiB each (at
   most 4 MiB of scratch in all), so the shared pool stays useful. Exact cells, dimensions and
   resource limits are preserved.
6. A narrow source-only RAW render path resolves invariant layout once per row. Normal validation
   and compilation, every supported source orientation and crop, f64 source adjustments, terminal
   encoding, cancellation, metadata and the generic fallback stay in place.

**Exactness.** Exact numerical changes are checked against independent canonical references,
representable neighbours of every relevant threshold, error and cancellation cases and complete
photo-sized output comparisons. Source preparation is exercised through new import, current and
historical saved white balance, reload, interpretation and fingerprint mismatch and a concurrent
white-balance change. Spatial estimates prove that preparation and reduction are actually avoided
and correctly invalidated. DNG compares serial and pool output and supplied-file samples. The
overlay compares every cell against a serial oracle. RAW rows compare generic evaluation, samples
and buffers across views and exposure.

**Review checklist.**

- Reads, hashes and decode stay on the signature-verified source worker; direct-at-target white
  balance removes a redundant development.
- The quantizer adds a bounded static table; DNG keeps one bounded scratch plane; clipping removes
  transient display-grid copies and bounds tiny-grid scratch to 4 MiB; the RAW row path adds no
  frame-sized scratch.
- Samples stay exact and use the existing bounded spatial tile; skipping irrelevant estimates
  removes work without rasterizing a frame.
- The owner performs only catalog, identity and validation work and job coordination; no new owner
  frame work.
- Desktop state, history, preview and upload paths and timers are unchanged.
- Existing output sharing and resource limits are kept and checked by targeted tests.

### Prepared rendering and native DNG development

Core render times include output allocation/drop and exclude source preparation, histogram reduction,
queue delay and presentation. Generated JPEGs are 6000 × 4000 and 10000 × 6000. The prepared linear
workload converts those JPEG pixels into planar floats before timing and applies +0.7 EV; it does not
measure camera decoding. Full Basic uses ten non-neutral fields; Mixer uses red hue +30, orange
saturation +20 and blue luminance −30. The proxy is 2700 × 1800, prepared outside timing.
Only the Air 2S row develops an actual retained native mosaic, including required optical corrections;
it excludes file read/unpack. Gains overlap and must not be added.

| Workload | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| Air 2S, native retained-mosaic development | 1318.3 / 1325.3 ms | 350.3 / 360.9 ms | 73.4% |
| 24 MP prepared linear, source exposure | 60.1 / 63.0 ms | 21.4 / 23.9 ms | 64.3% |
| 60 MP prepared linear, source exposure | 162.6 / 178.5 ms | 58.3 / 60.9 ms | 64.2% |
| 24 MP full Basic | 123.3 / 145.7 ms | 110.8 / 120.3 ms | 10.1% |
| 60 MP full Basic | 325.6 / 371.3 ms | 277.6 / 284.8 ms | 14.7% |
| 24 MP Mixer | 129.5 / 134.5 ms | 112.0 / 136.6 ms | 13.5% |
| 60 MP Mixer | 311.9 / 320.4 ms | 268.2 / 275.7 ms | 14.0% |
| 24 MP source, full Basic proxy | 26.6 / 30.6 ms | 22.8 / 24.8 ms | 14.4% |

Every full RGBA hash and the complete native planar-float hash matches before/after. The 24 MP
Mixer p95 is slightly worse despite its median gain; no tail improvement is claimed there. Serial/pool
DNG tests preserve every float bit, stage/channel order, active-area borders, failures and cancellation.
A separate ten-observation threshold diagnostic gives median 56.7 / 6.8 ms serial/pool at one megapixel
and 170.3 / 19.5 ms at three megapixels for synthetic gain/warp/vignette; inputs below one megapixel
remain serial. This small diagnostic is threshold feedback, not a p95 distribution.

### Spatial amount edits and clipping

A spatial row is one public point sample, including its bounded tile, immediately after changing only
the named strength from 60 to 61 on an already-sampled recipe. Linear inputs are prepared from JPEGs.
Texture/Clarity need no reduction even on the first sample; Dehaze still builds an atmosphere on a cold
cache or changed input. These savings did not remove the tile, which the point worker evaluated; the
point path they measured is deleted, and a sample is now a read by the tile service.

| Workload | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| 24 MP JPEG, Clarity | 27.2 / 28.5 ms | 12.6 / 12.9 ms | 53.5% |
| 24 MP JPEG, Dehaze | 19.5 / 20.6 ms | 5.7 / 6.0 ms | 70.7% |
| 24 MP linear, Clarity | 40.3 / 42.8 ms | 14.7 / 15.0 ms | 63.5% |
| 24 MP linear, Dehaze | 32.2 / 34.3 ms | 6.8 / 7.1 ms | 78.8% |
| 60 MP JPEG, Clarity | 53.8 / 57.0 ms | 19.1 / 19.6 ms | 64.5% |
| 60 MP JPEG, Dehaze | 43.0 / 48.2 ms | 7.0 / 7.3 ms | 83.7% |
| 60 MP linear, Clarity | 86.0 / 89.2 ms | 22.5 / 22.9 ms | 73.8% |
| 60 MP linear, Dehaze | 72.7 / 76.2 ms | 8.4 / 9.5 ms | 88.5% |

The clipping rows include reduction and output allocation, excluding decode, painting, upload and
queueing. Every cell matches the retained baseline. A 4096 × 2458 grid holds 9.6 MiB; it is no longer
replicated per parallel fold. Tiny grids remain essentially flat (within 0.2 ms in these medians).

| Workload | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| 24 MP → 1716 × 1144 grid | 30.5 / 36.3 ms | 1.9 / 2.8 ms | 93.8% |
| 60 MP → 1716 × 1030 grid | 30.9 / 36.6 ms | 4.3 / 4.5 ms | 86.1% |
| 60 MP → 4096 × 2458 capped grid | 276.8 / 319.0 ms | 4.3 / 4.8 ms | 98.4% |
| 60 MP → 1 × 1 grid | 4.4 / 4.8 ms | 4.4 / 4.6 ms | -0.6% |
| 60 MP → 8 × 8 grid | 4.2 / 4.6 ms | 4.4 / 4.7 ms | -3.8% |

### Integrated editor diagnostic

The official `editor-performance` comparison also uses 30 observations per variant and size in ABBA
order. These recipes include the harness’s composed transforms and 10° crop, so their values are
separate from bare kernels. Source-preservation checks pass. Leg-start load was 7.1–15.4; untouched
geometry/histogram medians move by roughly 0–4%, so smaller shifts are not attributed to a change.

| Workload | Before p50 / p95 | After p50 / p95 |
| --- | --- | --- |
| 24 MP source, Full Basic + geometry | 80.4 / 93.3 ms | 72.3 / 78.8 ms |
| 24 MP source, Exposure + geometry | 37.8 / 43.3 ms | 31.0 / 33.1 ms |
| 24 MP source, Full Basic proxy + geometry | 48.9 / 50.6 ms | 44.2 / 47.7 ms |
| 24 MP source, Build display-bounded proxy | 22.2 / 28.7 ms | 14.8 / 19.7 ms |
| 60 MP source, Full Basic + geometry | 189.7 / 197.8 ms | 173.8 / 178.9 ms |
| 60 MP source, Exposure + geometry | 89.7 / 94.5 ms | 72.0 / 78.2 ms |
| 60 MP source, Full Basic proxy + geometry | 52.7 / 62.7 ms | 46.5 / 50.7 ms |
| 60 MP source, Build display-bounded proxy | 33.1 / 36.7 ms | 24.4 / 30.0 ms |

### Cold saved-white-balance preparation

Every observation restarts the owner and source cache, with the filesystem cache warm, and opens an
existing catalog whose RAW has a saved custom red gain (1.1 × as-shot). The timer starts immediately
before `catalog.import` and ends when a strict exact-source `PreviewJob` is available, including job
waiting and adoption. Owner startup/catalog open, float hashing, final rendering and desktop
presentation are excluded. Full planar float bits are hashed after **every** observation. Source jobs
fall from two to one in every sample; hashes match for every before/after sample. Leg-start load
was 3.1–14.4.

| Source | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| Fujifilm X100VI | 2950.1 / 3013.8 ms | 1552.4 / 1605.4 ms | 1397.7 ms, 47.4% |
| DJI Air 2S | 2824.9 / 2859.5 ms | 485.8 / 517.2 ms | 2339.1 ms, 82.8% |

The Air 2S saving combines removal of its redundant development with parallel optical correction;
it must not be added to the isolated development saving above. Native correctness regressions also
cover Nikon Z6, new as-shot import, two distinct custom historical/current WB targets, source and
interpretation mismatch, same-target deduplication, different-target cancellation and a WB edit
while loading. Old gains never satisfy a strict request for the newer edit.

### Native qualification

All 38 verification components have passing evidence: workspace/API checks, 30 rendered scenarios,
RAW numerical references, three actual editor/open/edit/reopen trials per supplied Nikon, Fuji and DJI
file, and timing journeys. The initial full run passed 36 components. Its GPU-counter test read a
cached client list before its measured queue existed and could stop on a bookkeeping increment;
the fixture now warms that queue before a fresh sampler discovers it and waits for the existing
numerical lower bound. The bounds, timeout and allocation checks remain unchanged. The focused test
and final quick tier pass. The other failure was mask-range's draft-frame assertion during the
three-scenario pool: exact draft renders were cancelled by later inputs while pixel/state checks
passed. The isolated scenario passes on the identical production binary, displaying 11 drafted
frames. Original failures and focused reruns are retained in the local evidence, with a combined
`qualification.json`; passing native components were not rerun without a code change.

Final native Presence sample/render tests also pass on all three originals. DJI reopen, Presence and
mask/range captures were visually inspected alongside state and event checks. The full run's standard
timing components started above load 8.0 and their absolute target verdicts remain **unreliable**.
GPU colour and larger tiles remain separate work;
Windows/Linux numerical and native GPU qualification are not established by this M4 evidence.

## Startup and RAW throughput

Initial-source preparation now overlaps platform startup. RAW camera conversion mutates the existing
planes in exact bounded chunks, mosaic normalization and the output scale use bounded 16-row jobs
above one megapixel, and native Markesteijn and Bayer RCD use bounded tile jobs on the shared pool.
See the [contract and review checklist](#startup-and-raw-throughput-contract-and-review) and
[native execution bounds](../design/native-demosaic-parallelism.md). No image equation or output
quantization tolerance changes.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, release with locked pins,
25 September 2026. Baseline is main `5d121d7`; matrix/startup observations use `84370ec`,
whose timed matrix and first-proxy implementations are unchanged by the final native scheduler
and evidence-capture fixes. Final native observations and desktop qualification use `2dd1b72`.
The JPEG rendering control uses `59cf51d`; that timed implementation is unchanged. Each comparison
combines 15 observations per leg in before/after/after/before order:
30 per variant, with tails retained. Builds and tests stop during timing. Commands, hashes, load,
raw samples and captures are retained locally in `artifacts/performance-third-wave/`.
Scopes below overlap and their savings must not be added.

### Startup and RAW throughput: contract and review

These changes relax no numerical contract, change no module and add no runtime dependency or
scheduler. Source timings, render kernels and first-frame timings are distinct, and their savings
overlap.

1. The requested command-line image's bounded preparation job starts before platform and
   event-loop initialization. It reuses the desktop client and carries the job or its error into the
   ordinary open and adopt path. Catalog identity, generations, cancellation, original
   verification, history and error presentation stay authoritative. An empty launch starts no image
   job, and no source pixels are read or processed on the UI thread or the catalog owner.
2. Independent Markesteijn tile groups run on the shared Rayon pool through a synchronous private
   native callback, as specified in [bounded native demosaic parallelism](../design/native-demosaic-parallelism.md).
   CFA phase, global tile origins, equations, per-pixel order, border handling and one output frame
   are kept. Concurrent scratch is bounded, a job holds at most eight ordinary tiles and at most
   eight jobs run at once on the refill executor; the dependent final job runs on the source caller
   first, native failures and cancellation are handled, and there is no unsafe lazy shared
   initialization. Bayer RCD uses the same executor.
3. The core RAW camera matrix runs in exact disjoint 65,536-pixel chunks through the same refill
   executor above one megapixel and in serial chunks below it, and adopts already-validated planes
   through a private boundary that keeps the dimension and capacity checks. Public constructors
   still reject non-finite input. Cancellation and arithmetic order are preserved, with no extra
   full-frame allocation.

**Exactness and liveness.** Native complete float buffers agree byte for byte with the serial
reference at one, two, four and the admitted maximum workers, including custom white balance, odd
edge tiles and concurrent callers; original hashes, RAW history and reopen, and sample/render
parity stay intact. Worker count and aggregate explicit scratch are bounded. Cancellation, failures
and teardown publish no partial frame, leak no work and never unwind across FFI, and no callback
outlives the native call. Startup keeps the ordinary open behaviour: missing or invalid files report
normally, a replacement cancels stale work, one source job is adopted once, an existing saved white
balance is respected, and headless and background behaviour is unchanged. Unsupported platforms and
absolute targets under load remain unqualified.

**Review checklist.** Source work stays on the bounded worker. The native executor borrows the
immutable mosaic and context, has disjoint output interiors and reuses admitted per-callback scratch
across its tiles, then joins before borders. The core conversion mutates its existing three planes
in place. Startup overlaps an existing job rather than creating a second service or cache. Pixel
arithmetic, default previews, exact analysis and API/UI history keep their contracts.

### Exact camera conversion

The timer covers camera-to-working-space conversion and adoption validation over actual developed
camera planes. Allocation/clone, file read/decode, native development, hashing and drop are outside
it. The baseline uses the previous scalar expression and public validating constructor; the new
measurement invokes the actual private producer and adoption boundary. Complete float hashes match.

| Camera | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| Nikon Z6 | 50.73 / 51.66 ms | 3.74 / 4.63 ms | 92.6% |
| Fujifilm X100VI | 84.88 / 86.26 ms | 6.04 / 7.62 ms | 92.9% |
| DJI Air 2S | 42.02 / 42.87 ms | 2.98 / 3.91 ms | 92.9% |

The conversion writes disjoint 65,536-pixel chunks in place, as jobs of the development executor
([native demosaic parallelism](../design/native-demosaic-parallelism.md#why-callbacks-are-bounded)) above one
megapixel and serial chunks below. Each output is checked finite at production; private adoption
retains shape/capacity checks without another full scan. Public constructors still scan and reject
invalid input. There is no additional full-frame scratch. Leg-start load was 3.0–5.5.

### First image during startup

These are app-cold, filesystem-warm launches of temporary copied background bundles with a hidden
Metal window. An independent 10 ms event-file observer measures from argument parsing to the first
observed proxy event. This includes event observation delay; it is not scanout, foreground activation
or a stable installed-bundle launch. Ordinary first-preview behavior is unchanged by the later
capture-readiness fix.

| Source | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| Generated 24 MP JPEG | 806.0 / 828.1 ms | 764.0 / 778.6 ms | 42.0 ms, 5.2% |
| Generated 60 MP JPEG | 876.8 / 912.2 ms | 778.6 / 806.3 ms | 98.2 ms, 11.2% |

The existing authorized import starts before platform initialization and is adopted through the
ordinary open lifecycle; no second source job is created. Empty args-to-observed-capture remains
786.7 / 800.6 ms before and 787.5 / 809.2 ms after. Args-to-editor-startup remains about
578–582 ms. Leg-start load was 4.2–5.9. The initial-open request clock now starts before prequeueing;
old and new `open_to_raster` values therefore have different origins and are not compared.

### Native development and saved-white-balance preparation

Retained-mosaic development includes native output allocation/drop and excludes file read/unpack,
core matrix conversion and rendering. Fuji uses the final tile scheduler; Nikon and DJI native
implementations are unchanged. Complete native reference and before/after warm-output hashes agree.

| Native source | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| Nikon Z6 | 268.8 / 272.2 ms | 268.6 / 270.7 ms | Essentially unchanged |
| Fujifilm X100VI | 1277.2 / 1282.5 ms | 355.8 / 363.6 ms | 72.1% |
| DJI Air 2S | 351.9 / 368.0 ms | 350.6 / 358.1 ms | Essentially unchanged |

Cold saved-WB preparation restarts the owner and source cache per observation, with the filesystem
warm and the catalog's saved red gain at 1.1 × as-shot. Time runs from `catalog.import` until a
strict exact-source `PreviewJob` is available: read/hash/unpack/develop/matrix/validation and
waiting/adoption are included. Owner startup/catalog open, final float hashing, rendering and
presentation are excluded. Every observation's complete float planes and source identity match;
there is exactly one source job in every before/after sample.

| Source | Before p50 / p95 | After p50 / p95 | Median time saved |
| --- | --- | --- | --- |
| Nikon Z6 | 596.3 / 625.2 ms | 546.5 / 584.6 ms | 8.3% |
| Fujifilm X100VI | 1542.1 / 1574.4 ms | 535.9 / 572.8 ms | 65.3% |
| DJI Air 2S | 486.7 / 505.8 ms | 443.0 / 466.2 ms | 9.0% |

The unchanged-camera comparisons ran at load 3.2–6.5; final Fuji comparisons ran at load
2.3–4.3. Fuji development's process CPU median per 15-sample leg increases from
1276–1278 ms to 1512–1517 ms while wall time falls. Peak process RSS from those whole diagnostic
processes increases from 917.3 MB to 933.2–933.4 MB (decimal bytes); this includes preparation and
measurement, not just live demosaic scratch. The faster development uses about 19% more process CPU.

Explicit Markesteijn heap scratch is capped at eight × 988,208 bytes, or 7,905,664 bytes, for the
one development the source worker runs at a time, including the source-caller job. Stack arrays,
tables, allocator overhead and full image buffers are additional. Callback cancellation, native faults, nested callers and teardown are tested;
no partial output is adopted.

### Bayer RCD tile jobs

RCD runs its 194 px tiles as jobs of the same executor as Markesteijn, checking cancellation before
every tile ([native demosaic parallelism](../design/native-demosaic-parallelism.md)). Each job holds
one 978,536-byte scratch set under the same eight-lane cap. Complete RGB planes match the pre-change
serial digest on the Z6 and Air 2S and the serial raster at every worker count.

Native M4 Pro, 14 cores, release, 26 September 2026, one-minute load 3.2 to 4.1. Each run is the
crate's `bayer_normalization_release_abba_profile`: 30 warm observations per arm in 15 ABBA pairs in
one fresh process. The build before pooled RCD and this build ran back to back and then reversed
(before, after, after, before) per camera. The executor arm is the production path. Timing covers
retained-mosaic development: normalization, demosaic, output allocation and the final divide; it
excludes source read and decode, the DNG corrections, rendering and presentation. Both builds
normalized natively; normalization now runs in Rust ([mosaic normalization](#mosaic-normalization)).

| Source | Build | Wall p50 / p95 | Demosaic p50 / p95 | Process CPU p50 / p95 |
| --- | --- | ---: | ---: | ---: |
| Nikon Z6 | before | 237.8 / 240.8, 238.7 / 247.4 ms | 203.8 / 205.9, 204.1 / 212.6 ms | 290.5 / 296.0, 291.7 / 301.2 ms |
| Nikon Z6 | after | 55.9 / 58.3, 56.0 / 62.2 ms | 40.8 / 42.9, 40.8 / 44.9 ms | 301.3 / 307.2, 300.5 / 319.1 ms |
| DJI Air 2S | before | 199.0 / 201.8, 199.2 / 202.7 ms | 168.6 / 171.1, 168.9 / 171.2 ms | 250.1 / 253.7, 249.9 / 253.7 ms |
| DJI Air 2S | after | 44.2 / 45.1, 44.3 / 46.1 ms | 30.7 / 31.0, 30.6 / 31.1 ms | 256.0 / 262.3, 257.4 / 262.6 ms |

Development wall p50 falls by about 77% on both cameras and the demosaic by about 80%, for about 3%
more process CPU. Concurrent Fit proxies against a pooled RCD development are not yet measured. To repeat, run each
build's profile once per camera in the order before, after, after, before:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_PROFILE_SOURCE=nikon_z6.NEF \
LUXFORGE_RAW_TIMING_OUTPUT=/path/to/z6.csv \
  cargo test --release -p luxforge-raw --locked --lib -- --ignored --exact \
  tests::bayer_normalization_release_abba_profile
```

### DNG GainMap taps

A GainMap stage derives each active row's and each active column's bilinear taps (two map indices
and a weight) once, then weighs every pixel of every channel by the same f64 expression in the same
order, so the corrected planes are unchanged by construction. The Air 2S's complete corrected-plane
digest is the same before and after, in every observation.

Native M4 Pro, 14 cores, macOS 26.5.2, Rust 1.94.0, release with locked pins, 28 September 2026,
before at `ec132e71` with only the timing test added. Each run is the crate's
`owner_development_timing`: one discarded warm-up, then 30 observations of the production
development followed by its corrections, in one fresh process. Correction time covers GainMap,
WarpRectilinear and the copy back, over the three camera planes developed in that observation;
file read, decode, development, hashing and drop are outside it. The two builds ran before, after,
after, before, twice, on a shared host at one-minute load 6.4 to 18.4, so these are relative figures.

| Pass, order before-after-after-before | Before p50 / p95 (load) | After p50 / p95 (load) |
| --- | ---: | ---: |
| First | 164.5 / 403.7 ms (6.4), 112.6 / 141.5 ms (10.5) | 86.5 / 89.7 ms (13.9), 86.7 / 93.7 ms (11.9) |
| Second | 132.7 / 391.8 ms (10.6), 167.3 / 333.7 ms (13.6) | 92.8 / 100.3 ms (18.4), 95.0 / 262.6 ms (14.4) |

The least-disturbed pair, before at load 10.5 and after at 11.9 in the first pass, gives a
correction p50 of 112.6 against 86.7 ms, 23% less; every after p50 is below every before p50.
To repeat, run each build once per order:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_PROFILE_SOURCE=mavic_air_2s.DNG \
  cargo test --release -p luxforge-raw --locked --lib -- --ignored --exact \
  tests::owner_development_timing --nocapture
```

### Development executor refill

Native demosaic jobs, the mosaic normalization and output scale, DNG correction rows and
camera-conversion chunks all run on one development executor ([native demosaic parallelism](../design/native-demosaic-parallelism.md)):
the caller runs the final job first and then pulls ordinary jobs, while each pool lane runs one job
and re-spawns itself for the next, so no job waits for a joined batch's slowest job. Native jobs
keep the eight-lane cap; the Rust passes run at the pool's width. Every job still starts at a full
tile with fresh scratch, so the planes are unchanged: each source's complete development digest is
the same before and after in every observation.

Native M4 Pro, 14 cores, macOS 26.5.2, Rust 1.94.0, release with locked pins, 28 September 2026,
the crate's `owner_development_timing` as in [DNG GainMap taps](#dng-gainmap-taps): 30 observations
per run after one warm-up, each run a fresh process. Development covers normalization, demosaic,
output allocation and final divide, and excludes file read, decode, DNG corrections, the camera
conversion, hashing and drop. Before is `ec132e71` with only the timing test added for the Z6 and
X100VI, and the GainMap change for the Air 2S. Each pair of builds ran before, after, after, before
and then after, before, before, after, on a shared host.

| Source | Before p50 / p95 (load) | After p50 / p95 (load) |
| --- | ---: | ---: |
| Nikon Z6, first pass | 56.2 / 56.8 ms (7.1), 64.4 / 207.0 ms (6.4) | 43.1 / 43.6 ms (5.9), 43.7 / 76.6 ms (5.0) |
| Nikon Z6, reversed | 56.4 / 60.9 ms (11.8), 56.5 / 59.5 ms (10.6) | 44.6 / 95.1 ms (7.6), 43.2 / 44.2 ms (8.1) |
| Fujifilm X100VI, first pass | 523.4 / 995.3 ms (10.2), 452.4 / 1391.3 ms (16.5) | 302.2 / 467.6 ms (18.9), 291.6 / 590.7 ms (14.9) |
| Fujifilm X100VI, reversed | 330.7 / 335.2 ms (6.9), 335.4 / 929.7 ms (5.3) | 268.7 / 390.6 ms (6.9), 285.6 / 720.7 ms (9.8) |
| DJI Air 2S, first pass | 45.2 / 45.8 ms (7.5), 45.8 / 48.0 ms (8.3) | 37.6 / 38.9 ms (6.4), 40.7 / 93.0 ms (7.2) |
| DJI Air 2S, reversed | 45.4 / 49.1 ms (8.5), 45.2 / 46.0 ms (7.3) | 38.0 / 74.7 ms (7.4), 37.7 / 40.6 ms (6.1) |

Every after p50 is below every before p50 for each camera. In the least-loaded runs development p50
falls by about 23% on the Z6 (56.2 to 43.1 ms), 17% on the Air 2S (45.2 to 37.7 ms) and 17 to 19%
on the X100VI (330.7 and 335.4 against 268.7 and 285.6 ms); the X100VI's first pass ran at load
10 to 19 and is not used for the estimate. The p95 figures carry the host's load and no tail claim
is made. Concurrent Fit proxies against a refilled development are not measured.

The Rust passes were moved onto the executor on measurement, not only for the bound. Air 2S
correction p50 in the same runs is 83.0, 106.6, 84.7 and 84.2 ms on the executor against 86.5,
92.3, 86.8 and 87.3 ms on the parallel iterator it replaces; the one slower executor run has a p95
of 204 ms and ran during a load spike, so the two are equal or the executor slightly faster. The
Rust passes run at the pool's width, as the parallel iterator did: the eight-lane cap bounds native
scratch, which they do not hold. Four exploratory Air 2S runs at eight lanes, at load 15 to 20, were
too disturbed to compare (p50 112 to 174 ms at eight lanes against 83 to 207 ms at the pool's
width). The
X100VI camera conversion in core's `camera_conversion_photo_timing`, one build switching between
the two paths, 30 samples per run in parallel, executor, executor, parallel order at load 10 to 19,
gives a p50 of 6.66 and 6.19 ms on the parallel iterator and 6.71 and 6.26 ms on the executor,
with the same output digest: equal. Neither Rust pass holds per-job scratch; a camera-conversion job
is one 65,536-pixel chunk and a correction job 16 rows.

### Mosaic normalization

A development whose sensor stage rewrites the normalized values (a DNG stage-one vignette or
stage-two gain maps, or sparse repairs) normalizes the retained mosaic in Rust before the native
call, which only demosaics; every other development hands the native demosaic the per-site tables
below and allocates no normalized mosaic ([the RAW float mosaic](../design/efficiency.md#the-raw-float-mosaic)).
Either divides the demosaiced planes by the 65535 sensor scale in Rust after it
([native demosaic parallelism](../design/native-demosaic-parallelism.md#execution-boundary)). Both
Rust passes run in 16-row jobs on the development executor at the pool's width above one megapixel, for
Bayer and X-Trans alike, and in order on the caller below it; cancellation is checked before every
normalization row and every scale job. Each site's black level, `65535 / (white - black)` and gain
are computed once per site of the CFA and black-repeat period, checked there, and applied as
`((sample - black) * scale) * gain` in `f32`, the order the native loop used. The normalized mosaic
is allocated without being zeroed, each value written once by the job that owns its rows (a
repaired site twice), and released before the output scale; the scale is one pass over the three
contiguous planes. The neutral picker reads the same per-site black model. The Rust normalization
matched the native one bit for bit on synthetic Bayer and X-Trans mosaics, including a 1203 × 877
X-Trans frame, whose digests stay pinned; every owner source's complete development digest is the
same before and after in every observation below.

Native M4 Pro, 14 cores, macOS 26.5.2, Rust 1.94.0, release with locked pins, 28 September 2026,
the crate's `owner_development_timing` as in [DNG GainMap taps](#dng-gainmap-taps): one discarded
warm-up, then 30 observations per run, each run a fresh process. Development covers normalization,
demosaic, output allocation and the final divide, and excludes file read, decode, the DNG
corrections, the camera conversion, hashing and drop. Before is `5fbd54a2`, the development executor
refill, with native normalization; after is this change. Each camera ran before, after, after,
before and then after, before, before, after, twice, on a shared host; the one-minute load is given
for each run.

| Source | Before p50 / p95 (load) | After p50 / p95 (load) |
| --- | ---: | ---: |
| Fujifilm X100VI, first pass | 350.8 / 462.1 (51.0), 338.2 / 363.0 (22.7), 350.9 / 481.3 (20.3), 342.4 / 659.4 (19.6) ms | 222.9 / 241.8 (36.6), 227.0 / 254.3 (26.1), 237.8 / 348.6 (19.8), 228.8 / 291.8 (18.7) ms |
| Fujifilm X100VI, second pass | 271.5 / 708.9 (14.1), 281.5 / 387.4 (16.0), 280.6 / 536.3 (9.4), 291.6 / 647.6 (12.9) ms | 202.8 / 285.7 (21.4), 232.3 / 338.1 (17.9), 183.8 / 202.1 (13.8), 195.1 / 203.4 (11.9) ms |
| Nikon Z6, first pass | 50.9 / 65.5 (18.8), 51.1 / 58.1 (13.6), 51.9 / 57.2 (13.9), 52.1 / 59.2 (13.9) ms | 42.3 / 47.8 (16.3), 43.1 / 45.2 (14.9), 43.4 / 57.9 (14.6), 42.5 / 54.5 (13.9) ms |
| Nikon Z6, second pass | 44.1 / 62.6 (10.2), 47.7 / 96.8 (10.5), 44.1 / 78.0 (11.0), 43.6 / 74.4 (13.3) ms | 39.7 / 72.0 (9.1), 37.0 / 37.8 (12.7), 36.9 / 40.1 (12.4), 37.8 / 56.0 (13.5) ms |
| DJI Air 2S, first pass | 44.3 / 48.3 (13.0), 45.5 / 54.8 (16.0), 46.6 / 50.4 (17.0), 42.9 / 56.3 (17.2) ms | 35.6 / 39.4 (14.9), 35.1 / 39.5 (16.5), 36.0 / 39.1 (15.8), 33.9 / 37.8 (14.3) ms |
| DJI Air 2S, second pass | 37.9 / 40.9 (11.7), 38.0 / 39.1 (15.2), 44.4 / 93.6 (12.0), 85.0 / 128.5 (14.8) ms | 31.4 / 43.7 (10.5), 33.8 / 77.7 (13.2), 31.3 / 50.5 (13.2), 31.7 / 33.4 (22.2) ms |

Within each pass every after p50 is below every before p50 on every camera. In the second, less
loaded pass development p50 falls from 271.5 to 291.6 ms to 183.8 to 232.3 ms on the X100VI (about
30%), from 43.6 to 47.7 ms to 36.9 to 39.7 ms on the Z6 (about 15%) and from 37.9 to 38.0 ms to
31.3 to 33.8 ms on the Air 2S (about 17%, leaving out the 85.0 ms before run, taken during a load
spike). The X100VI gains most because its normalization was serial and it has the largest frame. The
p95 figures carry the host's load and no tail claim is made. Concurrent Fit proxies against this
development are not measured in the core; the last core-only same-pool Fit proxy figures predate
RCD's tile jobs ([further performance](../research/further-performance.md#bayer-normalization-batching)).
That contention is not measured through the editor: the whole-editor RAW measurement was closed
unrun on 2026-10-06.
The per-site table is at most one CFA and black-repeat period, each row widened to at least 64
sites: 396 sites for a 6 × 6 X-Trans period and 128 for a 2 × 2 Bayer one without a repeat pattern.

### Second development

The source cache keeps at most one more RAW development beside its current one, within
`luxforge_raw::RETAINED_DEVELOPMENT_BYTES` (600 MiB), so a switch between two entries at different
white balances is read back from memory instead of redeveloped
([second development](../design/raw-integration.md#second-development)). A completed source job
evicts the cached development only while a preparation of an original or a redevelopment is
queued; a queued artifact read leaves it.

Native M4 Pro, 14 cores, macOS 26.5.2, Rust 1.94.0, release with locked pins, 28 September 2026.
Each row is the `raw-editor` smoke scenario's edit launch, one background launch per observation,
from `raw-editor-checks.json`: request to the display event (`preview_displayed`, else
`render_ready`) and request to the correlated `frame_captured`. The journey previews the Original
from history and returns to current after six white-balance edits, then does the same pair again;
the first pair must develop the Original's white balance either way, and the second pair is the
hold-`\` compare once both developments exist. The capture includes the harness's window
readback, which is what the 100% view row costs with no render (its display is about 1 ms). Before
is the base `6c49e556` with the Masks-panel thumbnail fix (the base itself hangs on the first RAW
white-balance change, so no figure can be taken from it); after adds the eviction fix and the second
development. Arms alternate ABBA, 30 launches per arm on the X100VI and 10 on the Z6 and the Air 2S,
on a shared host whose one-minute load was 8.8 to 30.3 (X100VI, median 19.2 before and 19.8 after),
12.6 to 19.2 (Z6) and 16.1 to 20.3 (Air 2S). Values are nearest-rank p50 / p95 in milliseconds.

| Source, step | Before display | After display | Before capture | After capture |
| --- | ---: | ---: | ---: | ---: |
| X100VI, Historical Original | 251.5 / 492.9 | 242.7 / 378.1 | 314.8 / 572.3 | 303.2 / 508.9 |
| X100VI, Return to current | 249.8 / 464.8 | 20.6 / 71.7 | 359.4 / 678.6 | 131.5 / 255.7 |
| X100VI, Original again | 244.7 / 451.0 | 22.3 / 83.0 | 308.0 / 566.9 | 87.8 / 206.9 |
| X100VI, current again | 254.7 / 463.7 | 21.6 / 102.4 | 365.1 / 677.6 | 130.8 / 292.9 |
| X100VI, 100% view | 1.2 / 7.3 | 1.3 / 2.8 | 60.9 / 79.7 | 63.5 / 78.7 |
| Z6, Historical Original | 76.7 / 104.3 | 76.5 / 139.8 | 149.1 / 205.2 | 146.1 / 239.3 |
| Z6, Return to current | 80.0 / 142.0 | 34.4 / 70.0 | 161.0 / 317.5 | 100.8 / 165.6 |
| Z6, Original again | 85.3 / 117.8 | 31.1 / 54.1 | 145.2 / 233.4 | 99.1 / 143.8 |
| Z6, current again | 88.4 / 135.2 | 32.2 / 44.6 | 155.9 / 264.0 | 102.6 / 133.2 |
| Air 2S, Historical Original | 154.1 / 222.8 | 149.0 / 168.6 | 197.8 / 273.3 | 191.8 / 212.7 |
| Air 2S, Return to current | 150.6 / 269.8 | 20.2 / 42.0 | 214.8 / 422.3 | 81.0 / 135.3 |
| Air 2S, Original again | 148.4 / 263.1 | 20.1 / 63.9 | 196.2 / 353.1 | 63.8 / 122.8 |
| Air 2S, current again | 150.4 / 251.4 | 23.3 / 31.2 | 216.2 / 354.1 | 78.2 / 95.1 |

Without the second development every switch redevelops, and the X100VI's p50 is about 250 ms to
display and 310 to 365 ms to capture, above the owner's 150 ms threshold. With it, a switch whose
development the cache holds displays in about 20 ms p50 on every camera (X100VI p95 72 to 102 ms
under this load) and reaches the capture in 88 to 131 ms p50 on the X100VI, of which about 60 ms is
the readback; an undo, which also renders without redeveloping, measures the same in both arms
(X100VI capture 134.3 / 278.6 before, 129.5 / 320.1 after). The first Historical Original after a
white-balance change still redevelops, since the slot then holds the development before that
change, and is unchanged. The eviction fix alone changes none of these rows, as the journey queues
no artifact read: three ABBA launches per arm on the X100VI (load 14.1 to 19.1) displayed the
Historical Original in 324 to 352 ms before and 327 to 478 ms with the fix, and returned to current
in 317 to 372 and 315 to 435 ms.

Memory is the edit launch's process RSS, sampled about every 50 ms by `ps` from the second pair's
captured frame to the process's end (the 100% and Fit steps), with the current entry's development
and the Original's both held after; the figure is each launch's maximum over that window, p50 / p95
over the same launches. GPU resources, captures and allocator retention are included, not
separated.

| Source (development planes) | Before | After | Difference |
| --- | ---: | ---: | ---: |
| X100VI, full sensor (468 MiB) | 1832 / 1882 MiB | 2290 / 2352 MiB | +458 MiB |
| Z6, 6064 × 4040 sensor (280 MiB) | 1263 / 1300 MiB | 1527 / 1559 MiB | +264 MiB |
| Air 2S, 5568 × 3648 sensor (232 MiB) | 1220 / 1269 MiB | 1450 / 1484 MiB | +230 MiB |

The difference is the retained development's planes. A development past the 600 MiB budget, such as
one at the 128 MP admission limit (about 1.43 GiB of planes), is never retained, so such a
photograph keeps one development and redevelops on every switch as before. At a redevelopment's
peak the process now holds the retained development beside the one being built, so the X100VI's
white-balance-release peak rises by at most the same 468 MiB.

### RAW colour row batching

The production RAW renderer writes its last segment in the row chunks the JPEG colour pass uses
(at most 16 rows and 1 MiB of RGB float per chunk), on the shared Rayon pool above one megapixel,
checking cancellation and reserving the chunk's scratch from the colour budget per chunk. Every
stack evaluates its colour runs over those rows: masks, replacements, exact geometry and the segment
after a resample or a spatial operation as well as a source-only segment. The segments before a
boundary are pulled, never materialized, a bounded rectangle at a time with their colour run over
its rows: a spatial operation's input one row of its tile region at a time, and a straightened
crop's taps one block of 64 output columns by the chunk's rows at a time, through the rectangle of
at most 16,384 pixels those taps read, so each pixel before the resample is evaluated about once
per block instead of once per tap. A segment with a point replacement is pulled pixel by pixel.
Complete-buffer tests compare the rows with the point evaluator for Basic, Mixer and their combined
recipe, a viewed source, a partial final chunk, replacements on either side of a colour run, colour
after a straightened crop at 4 and -30 degrees and after Presence, masked colour and a masked
Presence layer before a crop, on a stage narrower than one tap block and on one several blocks
wide.

The measurements below were taken when only a source-only segment was batched, in eight-row
chunks; they are the evidence for that path. Release core-render comparison on retained real RAW
planes, 30 observations per variant and recipe in 15 ABBA pairs. The reference is the old per-pixel
evaluator with the same parallel scheduling threshold. Timers include output allocation and drop, but exclude file read, decode/development,
desktop scheduling, GPU upload and presentation. Process CPU is cumulative over the shared 14-core
Rayon pool, sampled outside each render timer. No build or test ran with the timed profiles.

| Camera / recipe | Reference wall p50 / p95 | Production wall p50 / p95 | Reference CPU p50 / p95 | Production CPU p50 / p95 |
| --- | ---: | ---: | ---: | ---: |
| Z6 / Full Basic | 398.3 / 449.0 ms | 146.0 / 155.2 ms | 5217.9 / 5290.4 ms | 1888.0 / 1949.4 ms |
| Z6 / Mixer | 297.0 / 308.9 ms | 167.9 / 175.8 ms | 3866.0 / 3912.6 ms | 2154.4 / 2190.5 ms |
| X100VI / Full Basic | 646.4 / 893.3 ms | 220.2 / 290.1 ms | 7903.9 / 8149.5 ms | 2598.8 / 2738.2 ms |
| X100VI / Mixer | 503.9 / 607.2 ms | 278.0 / 315.2 ms | 5793.8 / 5914.1 ms | 3127.7 / 3236.8 ms |

The first X100VI pass was lower-tailed: Full Basic reference/production p50/p95 618.8/700.3 ms and
201.2/252.7 ms; Mixer 496.8/557.0 ms and 277.1/315.2 ms. Leg-start load was 6.38 for Z6 and 5.39
and 5.80 for the Fuji passes. The renderer drove the shared pool; one-minute load at the end rose to
14.80, 24.37 and 23.82. The table gives the repeat Fuji distributions, including the wider tail.

The production median reduction is 63% on Z6 and 66–68% on X100VI for Full Basic, and 43–44% for
Mixer. These are separate recipe workloads; do not sum their savings into a combined-stack estimate.
Source planes are 294 MB and 491 MB, outputs 97 MB and 159 MB. Row scratch is at most 676 KB and
1.30 MB across 14 folders. Activity Monitor footprint after decode was 556 MB and 925–928 MB; the
reference and production snapshots were about 752 MB and 1.244–1.247 GB, with no material
production increase. Those snapshots are not allocator traces or GPU allocation measurements; the
full-output equality check holds both RGBA outputs together once.

The shared-pool Fit proxy check uses a 1920 × 1280 nearest-sampled proxy from actual Fuji retained
RAW planes and the Full Basic recipe. Thirty proxy requests are measured while one external caller
repeats exact full-source renders; proxy creation, RAW decode, build, desktop work, upload and
presentation are excluded. The generic exact renderer completed 30 full renders and the production
row path completed 29. The process memory footprint after overlap was 1.172 GB for the generic path
and 1.166 GB for production (resident 1.180 GB and 1.175 GB respectively). Process CPU over each
whole overlap window includes both the exact worker and proxy requests.

| Exact render sharing the pool | Proxy alone p50 / p95 | Proxy with exact work p50 / p95 | Overlap wall / process CPU |
| --- | ---: | ---: | ---: |
| Generic per-pixel reference | 13.9 / 26.5 ms | 623.9 / 658.3 ms | 18.8 s / 240.7 CPU s |
| Production row batching | 12.1 / 13.1 ms | 207.2 / 214.7 ms | 6.0 s / 79.9 CPU s |

Production cuts the contended proxy p95 by about 67%, but 215 ms still misses the 32 ms acceptable
input-to-presented-frame limit. A one-row callback experiment measured 213.5/226.2 ms p50/p95 and
30 exact renders in 6.4 seconds, slightly behind the eight-row path's 207.2/214.7 ms and 29 renders
in 6.0 seconds. This measures a continuously active core exact render and a core proxy, not a user's
UI gesture or GPU presentation. The external load at the starts of the two variant legs was 7.86
and 6.98; no build or test ran alongside either. The intentional shared-pool work raised the ending
loads to 8.91 and 9.57. Full scope and scratch accounting are in
[further performance opportunities](../research/further-performance.md#raw-colour-row-implementation-and-measurement).

### Shared-pool contention and rendering controls

One retained Fuji development competes with four full-Basic renders of a prepared 1920 × 1280
JPEG proxy. External callers start at a barrier and share Rayon. Each operation clock excludes
preparation, thread creation, hashing and frame drop; the proxy is hashed between requests while
RAW work continues. This is a CPU scheduling diagnostic, not desktop/GPU/presentation latency.
There are 30 development and 120 proxy observations per variant, in the same ABBA order; complete
proxy hashes match. Leg-start load was 2.1–5.2.

| Concurrent operation | Before p50 / p95 | After p50 / p95 |
| --- | --- | --- |
| Fuji retained-mosaic development | 1292.6 / 1303.8 ms | 367.8 / 379.7 ms |
| Full-Basic JPEG proxy render | 11.69 / 13.28 ms | 11.66 / 14.01 ms |

The throughput gain does not materially move the proxy median; its p95 increases by 0.74 ms.
Earlier schedules were rejected because a preview waiter could steal long native work: draining
row groups gave 216 ms proxy p95, one row per callback still about 64 ms, and a four-worker cap
still about 60 ms. Ordinary pool callbacks now contain at most eight tiles. The dependent final
two rows run on the existing external source caller, which holds nothing across a join.

A separate phase sweep starts preview work 0/75/125/175/225/275/325 ms after the RAW barrier,
five trials at each offset and four renders per trial. The seven 20-observation proxy p95 values
range from 12.3 to 17.6 ms; the maximum of all 140 renders is 27.0 ms. This diagnostic checks
later overlap as well as the ordinary early-start case; it is not a 30-sample distribution per
phase or a hard latency guarantee. Load was 2.8–3.4. Nested Rayon callers retain correctness and
liveness coverage, but the latency result applies to the production external source caller.

The official JPEG `editor-performance` diagnostic is a control: this batch changes no JPEG colour
kernel. Its composed geometry/crop and full Basic recipe produces the following 30-observation
results. Source-preservation checks pass. Leg-start load was 3.9–14.2; small shifts in unchanged
code are not attributed to an optimization or used as absolute latency-budget evidence.

| Workload | Before p50 / p95 | After p50 / p95 |
| --- | --- | --- |
| 24 MP full Basic + geometry | 78.7 / 88.0 ms | 78.2 / 81.2 ms |
| 60 MP full Basic + geometry | 183.0 / 191.1 ms | 182.5 / 188.9 ms |
| 24 MP source, full Basic proxy + geometry | 47.5 / 52.3 ms | 47.2 / 50.0 ms |
| 60 MP source, full Basic proxy + geometry | 50.0 / 52.0 ms | 49.3 / 53.1 ms |

### Qualification and remaining opportunities

The final full tier passes all 38 functional components: workspace/API checks, 30 background
rendered scenarios, RAW numerical references, nine actual RAW open/edit/reopen journeys and timing
journeys. Independent review covered tile scratch, disjoint writes, FFI lifetime, nested admission,
private validated adoption and capture settlement. Complete native float references, original
hashes and sample/render/history checks pass. Final Fuji and DJI reopened captures were visually
inspected alongside their correlated state and events.

An earlier run exposed an evidence race: a screenshot requested from the temporary 1× proxy could
return after the 2× refit was displayed, making identical recipes appear different on reopen.
Capture readiness now waits for permitted current-bounds refits, preserves intentional draft deferral
and render-error evidence, and retries readbacks superseded by newer photo pixels before publishing
or saving them. Ordinary first-preview presentation is unchanged. Focused draft/refit tests and the
final native journeys pass; the original failure and diagnosis remain in the local evidence.

The final full tier's `editor-latency` and `measure` components started above load 8.0, so their
absolute-budget verdicts remain **unreliable**. The separate comparisons above retain their own
loads, scopes and sample counts. No sanitizer run, Windows/Linux numerical qualification or
cross-platform native GPU qualification is claimed by this M4 evidence.

The remaining candidates are ranked in [further performance opportunities](../research/further-performance.md).
Mosaic normalization on the development executor and RAW colour row batching are in production and
measured on actual owner inputs; their core shared-pool results are separate from presentation and do not replace end-to-end
evidence. RCD runs bounded tile jobs; its quiet-host timing and preview contention remain to be
measured. Startup attribution, GPU texture transfer, GPU
execution and SIMD/assembly remain open; CPU RGBA publication already writes directly into the
frame owner, and no additional savings are assigned to it.

### JPEG export

Release `luxforge-json` on the owner's M4 Pro, 27 September 2026, warm file cache, driven over its line-delimited JSON by a throwaway script: `catalog.import`, the preparation job, one `edit.set-basic` committing `exposure: 0.3`, then five exports of that entry back to back, each timed from sending `export.jpeg` to `export.read` first answering `ready`, polled every 20 ms. Five samples give a median and a maximum, not a p95. No RAW export needed a preparation: the committed edit had already developed the entry's settings. The `image` column is the first build, which encoded with the `image` crate, at one-minute load 3.9 to 5.2; the libjpeg-turbo column is the current build, which encodes through `mozjpeg`, at one-minute load 2.8 after waiting for the host to settle (5- and 15-minute loads 10.8 and 13.7 from other sessions). The two runs were not back to back, so the comparison is indicative; the encoder alone, measured back to back, is 48 against 111 ms at 24 MP and 121 against 274 ms at 60 MP ([export design](../design/export.md#decisions)).

| Source | Export wall p50 / max of 5, libjpeg-turbo (ms) | Same, `image` (ms) | Output, libjpeg-turbo | Dimensions |
| --- | ---: | ---: | ---: | --- |
| 24 MP JPEG (generated) | 90 / 110 | 158 / 176 | 688 KiB | 6000 × 4000 |
| 60 MP JPEG (generated) | 170 / 175 | 363 / 386 | 1.63 MiB | 10000 × 6000 |
| Nikon Z6 NEF | 118 / 127 | 220 / 230 | 2.35 MiB | 4024 × 6048 |
| Fujifilm X100VI RAF | 185 / 193 | 331 / 338 | 4.36 MiB | 7728 × 5152 |
| DJI Air 2S DNG | 120 / 131 | 243 / 251 | 6.29 MiB | 5464 × 3640 |

Peak memory was measured with the `image` encoder only: `/usr/bin/time -l`'s maximum resident set size of two separate processes per source, one stopping after the edit and one exporting once. One export raised the process peak from 173 to 285 MiB at 24 MP and from 416 to 661 MiB at 60 MP, and by 1 to 8 MiB for the three RAWs, whose frame fits under the peak RAW development already reached. Each difference is how far one export raises the process's peak, not the export's own allocation. The libjpeg-turbo encoder streams 16 rows at a time into the file, as the `image` encoder streamed blocks, so it adds no whole-frame buffer; its peak has not been re-measured.

Every figure here is the reference renderer's export. The GPU export ([export](../design/export.md#behavior), stage 4 of [GPU-first](../design/gpu-first.md)), whose tiles the desktop's tile worker draws, is timed against the reference export at 24 and 60 MP on a trivial and a heavy stack in [export at 24 and 60 MP](#export-at-24-and-60-mp): on the heavy stack 0.95 s against 0.76 s at 24 MP and 10.4 s against 2.2 s at 60 MP. Its process peak memory and its export of the supplied RAWs are not measured.

### JPEG decode: libjpeg-turbo against zune-jpeg

Import decodes JPEG with libjpeg-turbo, through the same codec (`luxforge-jpeg`) as export, instead of `image` 0.25.9's zune-jpeg 0.5.15. Decode only, on the owner's M4 Pro, 27 September 2026, release test build, warm file cache, the file read once outside the clock and neither hashing nor capture metadata timed: `source::jpeg_tests::decode_timing`. The previous path is replayed as the import ran it (the header walk, `image`'s reader with its limits, its ICC and orientation reads, the decode to RGB, `apply_orientation` and the copy into the RGBA frame); the adapter path is `decode_upright`. Median of 7 after one warm-up, run twice back to back, the previous path first in each pair and then the adapter first; the two orders agreed within 0.6 ms. The host was shared: one-minute load 16 to 18, five-minute 30, from other sessions, so the absolute figures are indicative and the ratio is the result.

| Source | libjpeg-turbo (ms) | zune-jpeg path (ms) | Peak added, libjpeg-turbo | Peak added, zune-jpeg path | Dimensions |
| --- | ---: | ---: | ---: | ---: | --- |
| 24 MP JPEG (generated) | 22.7 | 30.9 to 31.5 | 92 MiB | 162 MiB | 6000 × 4000 |
| 60 MP JPEG (generated) | 55.2 to 55.3 | 74.7 to 74.9 | 230 MiB | 403 MiB | 10000 × 6000 |
| Drone photo, 10 MB | 75.0 to 75.5 | 103.1 to 103.3 | 55 MiB | 107 MiB | 3389 × 4236 |

Peak added is `/usr/bin/time -l`'s maximum resident set size of the test process decoding the file once through one path, less the same process decoding nothing. The adapter's is the RGBA frame itself (92, 229 and 55 MiB) plus libjpeg's working buffers, under 1 MiB: rows go straight into the frame, or through a 16-row strip when the EXIF orientation turns the image. The previous path also held the whole image as RGB before copying it. All three files are baseline with orientation 1; a progressive file adds libjpeg's coefficient buffer, 2 bytes per sample. x86_64 builds libjpeg-turbo's portable C path and is not measured.

Decoded pixels differ from zune-jpeg's slightly (`luxforge-jpeg`'s `tests::measure_the_difference_from_the_previous_decoder`): at most 3 codes on every committed fixture, 0.3% of pixels on the orientation set; on the generated 24 and 60 MP files 0.0014% and 0.0006% of pixels by at most 2 codes; on the drone photo 1.4% of pixels, 0.45% by more than one code and 0.002% by more than two, mean 0.01 codes per channel. The one large difference is the 4:2:0 file with a separate scan per component, which zune-jpeg misread.

### Exposure on RAW: source development against Basic

Host: the owner's M4 MacBook Pro, native, macOS 26.5.2, release profile with locked pins. Date:
27 September 2026. Commit measured: `ee5743e9`, before the RAW development's own exposure is
removed ([source-kind controls](../design/source-controls.md#decisions), decision 1). This is
image-difference evidence for that decision, not a timing measurement: no timing tool is
involved.

Scope: the three supplied RAW files (Nikon Z6 NEF, Fujifilm X100VI RAF, DJI Air 2S DNG), at EV
−2.37, −0.5, +0.01, +1 and +3.3. For each file and EV, (A) a RAW development at that EV with no
Basic layer is compared against (B) a development at 0 EV plus a Basic layer `{exposure: ev}`,
each drafted through `set-raw-exposure` and `set-basic` and rendered through the production
render entry point (`render`, `Render::frame`, `Render::render_proxy`) at a display-bounded proxy
stage (2048×2048 bounds, downscaled through `PreviewSource::proxy`) and at full size, to 8-bit
sRGB. Compared: the largest per-channel code difference and the share of R, G and B channel
samples (alpha is opaque on both sides and carries no exposure) more than one code apart, at
either size.

The measurement was the ignored `exposure_move_measure` test at commit `ee5743e9`, run once per
file with `LUXFORGE_RAW_FIXTURE` naming it. It was removed with the RAW development's exposure,
since side A no longer exists; check out `ee5743e9` to reproduce it.

| File | EV | Proxy stage | Proxy max Δcode | Full stage | Full max Δcode | Share beyond 1 code |
| --- | ---: | --- | ---: | --- | ---: | ---: |
| Nikon Z6 NEF | −2.37 | 1363×2048 | 1 | 4024×6048 | 1 | 0 |
| Nikon Z6 NEF | −0.5 | 1363×2048 | 1 | 4024×6048 | 1 | 0 |
| Nikon Z6 NEF | +0.01 | 1363×2048 | 1 | 4024×6048 | 1 | 0 |
| Nikon Z6 NEF | +1 | 1363×2048 | 0 | 4024×6048 | 0 | 0 |
| Nikon Z6 NEF | +3.3 | 1363×2048 | 1 | 4024×6048 | 1 | 0 |
| Fujifilm X100VI RAF | −2.37 | 2048×1365 | 1 | 7728×5152 | 1 | 0 |
| Fujifilm X100VI RAF | −0.5 | 2048×1365 | 1 | 7728×5152 | 1 | 0 |
| Fujifilm X100VI RAF | +0.01 | 2048×1365 | 1 | 7728×5152 | 1 | 0 |
| Fujifilm X100VI RAF | +1 | 2048×1365 | 0 | 7728×5152 | 0 | 0 |
| Fujifilm X100VI RAF | +3.3 | 2048×1365 | 1 | 7728×5152 | 1 | 0 |
| DJI Air 2S DNG | −2.37 | 2048×1364 | 0 | 5464×3640 | 1 | 0 |
| DJI Air 2S DNG | −0.5 | 2048×1364 | 1 | 5464×3640 | 1 | 0 |
| DJI Air 2S DNG | +0.01 | 2048×1364 | 1 | 5464×3640 | 1 | 0 |
| DJI Air 2S DNG | +1 | 2048×1364 | 0 | 5464×3640 | 0 | 0 |
| DJI Air 2S DNG | +3.3 | 2048×1364 | 1 | 5464×3640 | 1 | 0 |

Every integer EV (+1) is byte-identical at both sizes on all three files, as an exact power of
two multiplied in f64 (the source read) and in f32 (Basic's gain) must be. Every non-integer EV
differs by at most one 8-bit code, on a measured share of exactly zero R/G/B channel samples
beyond that one code, so within this scope moving Exposure from the source read's f64 multiply to
Basic's f32 `2^EV` gain is a sub-visible rounding difference, not a visible one, at either size on
any of the three files. The one asymmetry recorded is the DJI Air 2S DNG at −2.37 EV, where the
proxy pair is byte-identical (max Δcode 0) but the full-size pair differs by one code; both remain
within the one-code bound.

### Exposure drag on RAW: source development against Basic

Host: the owner's M4 MacBook Pro (Apple M4 Pro, Metal), native, release, background evidence
launch, warm file cache, 27 September 2026. Before: `ee5743e9`, dragging the RAW development's own
Exposure (`--action set-raw-exposure --parameter ev`). After: the one Basic Exposure on the same
photo (`editor-latency`'s default slider), on the integration build that removed the RAW exposure.
Scope: the supplied Nikon Z6 NEF at Fit, `editor-latency --mode drag --samples 30`, one launch per
run. The runs went before, after, after, before, back to back. Every run started with the host's
one-minute load between 7.1 and 10.1, above the [timing threshold](../engineering/development.md),
so these are relative figures on a shared host, not budget evidence.

| Run | Slider | Input to presented frame p50 / p95 (30 inputs) | Release to committed frame (2) | Release to settled histogram (2) | Sampled peak RSS |
| --- | --- | ---: | ---: | ---: | ---: |
| Before 1 | RAW development | 13.1 / 24.9 ms | 10.4, 40.2 ms | 52.1, 85.0 ms | 1418 MiB |
| After 1 | Basic | 8.5 / 10.6 ms | 16.2, 18.7 ms | 43.7, 50.0 ms | 1478 MiB |
| After 2 | Basic | 9.4 / 10.7 ms | 17.2, 23.7 ms | 49.8, 58.9 ms | 1414 MiB |
| Before 2 | RAW development | 11.8 / 17.6 ms | 10.5, 18.7 ms | 50.4, 56.2 ms | 1438 MiB |

Dragging Exposure on RAW presents faster in Basic's colour run than in the source read, in both
orders: p50 about 3 ms and p95 7 to 14 ms lower. With two commits a run, the release figures are
not a distribution. The committed frame read about 6 ms later after the move in both pairs, which
is noted rather than claimed.

### Native masking interaction qualification

The [masking interaction repairs](../design/masking-interactions.md) are measured on native Apple
M4 Pro/Metal, release, a 2880 × 1800 physical window at 2× scale, using background-only hidden
launches and isolated catalogs. The qualified executable SHA-256 is
`ea9f2813a5baf47513ffdc0edf827cc036624f6a91aec60964ecbe4bd7b8cbb9`; Cargo.lock is
`aa36c2217e8802001d585da67e2569499d2b39f1e107a0848b8f5ab2311b6263`.
Sources are the generated 6000 × 4000 and 10000 × 6000 JPEGs with hashes `b54c2a158a3d3846…`
and `b9e0118ab69b5d88…`. Source hashes remain unchanged. Filesystem reads are warmed by hash
verification; source preparation occurs at fresh-process open and is settled/warm during the
measured hover or stroke. These measurements do not describe hovering before a photo is loaded.
All final runs start below the existing load-8.0 threshold; maximum start load is 7.55.

There are 18 paced hover runs, 30 emitted moves each, at 16 ms intervals: 540 inputs with zero
coalescing. They route through the actual laid-out mask canvas and surrounding mouse area after
New Mask is armed. Every move reads the retained exact raster: zero point-query requests, draft
sets, photograph jobs, source decodes, photograph texture writes or photograph upload bytes.
The table is **CPU input-to-cursor-geometry submission**, p50 / p95 / maximum milliseconds,
n=30 per cell. Matched editor pointer updates and workspace derivation are recorded separately;
CPU geometry may precede the queued editor update.

| Hover workload | 24 MP | 60 MP |
| --- | ---: | ---: |
| Clarity +50, Tint, Fit | 4.01 / 8.58 / 8.66 | 3.46 / 7.81 / 8.49 |
| Clarity +50, Tint, 100% | 3.66 / 8.60 / 8.72 | 3.46 / 7.93 / 8.15 |
| Clarity +50, Tint, 200% | 4.08 / 8.14 / 8.64 | 3.47 / 7.32 / 8.50 |
| Clarity +50, 7° crop, 100% | 3.82 / 8.22 / 9.21 | 3.48 / 7.30 / 8.71 |
| Basic before Clarity, Tint, Fit | 3.89 / 7.83 / 9.00 | 3.58 / 8.14 / 8.69 |
| Masked Exposure +0.5 EV, Tint, Fit | 3.29 / 7.15 / 8.29 | 4.54 / 8.96 / 9.91 |
| Identity, Tint, Fit | 3.20 / 7.80 / 7.92 | 3.24 / 8.54 / 8.57 |
| Identity, Off, Fit | 3.50 / 7.98 / 8.13 | 3.03 / 7.87 / 7.93 |
| Clarity +50, Off, Fit | 3.75 / 8.14 / 8.41 | 4.04 / 10.34 / 11.99 |

A separate pair of sequential cursor runs checks 30 native renderer readbacks per source. Every
input epoch and coordinate match its captured state; both cursor rings pass independent
PNG probes (at least 64/64 outer probes and 45/64 inner probes). These 60 frames prove rendered
correspondence with capture overhead. They do not turn the paced CPU figures into GPU completion
or display-scanout latency.

The diagnostic before retained readout used 12 scheduled Fit moves over masked Clarity. At 24 MP,
8 were emitted and 4 coalesced, with 8 exact point queries and CPU geometry p50/p95 22.52/59.16 ms;
at 60 MP, 7 were emitted and 5 coalesced, with 7 queries and 52.85/81.18 ms. Start loads were 4.95
and 3.62. Cursor geometry construction and workspace derivation were small; tile-based exact point
sampling was the expensive path. These small diagnostic counts identify the mechanism. They are
not a matched 30-input before/after comparison of the whole repair or a renderer-kernel speedup.

Two new unbound-brush runs at 24/60 MP accept 30 positions each at 24 ms intervals, expose 29 live
coverage results, and commit one stroke/history entry. They submit zero photograph work or uploads
and preserve the photograph texture version. Seven bound-brush runs use one brush/component and a
masked Basic Exposure +0.5 EV layer, with Tint: Fit/24 MP, 100%/60 MP and 200%/24 MP short strokes,
and 240- and 400-position Fit/24 MP and 100%/60 MP holds. The short runs yield 29 draft feedback
samples; their final position releases/commits, so they are functional diagnostics rather than
30-sample timing claims. The longer runs qualify actual accepted-coverage readiness:

| Bound stroke | Photo / coverage samples | Input to authoritative coverage p50 / p95 / maximum ms | Sampled peak RSS |
| --- | ---: | ---: | ---: |
| Fit, 24 MP, 240 positions | 239 / 239 | 8.42 / 9.41 / 17.60 | 643 MiB |
| 100%, 60 MP, 240 positions | 239 / 239 | 7.91 / 9.26 / 27.17 | 1111 MiB |
| Fit, 24 MP, 400 positions, 9.6 s | 399 / 397 | 8.62 / 17.85 / 45.24 | 656 MiB |
| 100%, 60 MP, 400 positions, 9.6 s | 399 / 399 | 7.95 / 9.25 / 27.76 | 1195 MiB |

Feedback continues during every long hold. Latest pending work may replace a coverage revision;
the 24 MP 400-position count explicitly retains its two missing coverage pairs. Early/late
`draft.set` p95 is 0.0346/0.1442 ms at Fit/24 MP and 0.0345/0.0378 ms at 100%/60 MP, for the first
100 and last 99 accepted draft samples. Fit/24 MP late photo-adoption p95 rises to 32.78 ms, with a
62.37 ms maximum; this tail is retained. Capture/controller-to-draft dispatch p95 is separately
0.0163/0.0220 ms and 0.0129/0.0200 ms; that span includes preflight and payload setup, rather than
isolating raw capture. Tests prove the 16,384-position live bound and explicit refusal/recovery;
these 400-position measurements do not qualify worst-case native latency at that bound.

Peak sampled process RSS reaches **1640 MiB** in the cropped 100%/60 MP hover workload; the
bound-paint maximum is 1388 MiB in a short 100%/60 MP run. The 1 GiB provisional RSS reference is
exceeded. These capture-heavy editor measurements include backend resources, captures and allocator
retention; they do not establish the 600 MiB CPU-resident edit budget or qualify import/export
memory. Total editor/GPU memory accounting and bounds remain incomplete.

The 1000-position Fit stroke fails the existing 4096-event diagnostics ceiling (3979 events
dropped); its final/late measurements remain unavailable. The limit is unchanged and this is not a
passing stroke result. An initial 100% short-stroke report also failed because the timing parser
classified a region completion as exact. The narrow parser correction has two regressions and its
fresh native rerun passes on the same qualified product binary. Both failures remain in the evidence.

The aggregate report is `/private/tmp/mask-final-20260930/summary.json`, including every per-run
report path, quantile/sample count, load, RSS, counters and retained failure. Paced measurements use
`editor-latency --binary PATH --source fixtures/generated/24mp.jpg --output NEW_DIR --mode hover
--samples 30` and the corresponding 60 MP source, with `--zoom`, `--crop`, `--basic` or
`--action set-basic --parameter exposure` for the stated rows. Bound painting uses `--mode paint
--mask-overlay --samples 240` or `400`, with `--zoom 100` for the 60 MP viewport. Custom Off/identity,
unbound creation and sequential cursor proof scripts use `develop --background`; their exact scripts
and commands are retained alongside the reports.

The final quick tier and all 36 rendered scenarios pass; `mask-interactions` contributes 44
correlated frames and independent coverage probes. Separate supplied Z6 NEF, X100VI RAF and Air 2S
DNG checks pass 30 captures and seven exact white-balance redevelopments each, with unchanged source
hashes, correct history and zero black-mask probe drift. That establishes functional RAW liveness,
not RAW latency qualification. Native Windows/Linux, GPU completion/scanout latency, the total
memory budget and the failed 1000-position diagnostic remain unqualified. No full-tier or broader
editor milestone is claimed.

## RawSpeed unpacking

For 64 catalog modes RawSpeed fills the sensor mosaic inside LibRaw's unpack
([RawSpeed unpacking](../design/rawspeed-unpack.md)). A mode is routed only when its unpack is at
least 1.3× faster through RawSpeed; every exact candidate passed, so none was moved back to LibRaw.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, release with locked pins,
30 September 2026. Measured source: `7f4cac99` with only the three ignored measurement tests added;
no build or test of this worktree ran during a timing run, and other sessions shared the host.
Every p50 and p95 is nearest-rank, with no tails removed. Observations, per-run load and scripts
are retained locally under the measuring worktree's `target/task-007/`.

### Adapter unpack per routed mode

`rawspeed_unpack_timing` times `RawSource::decode` (identify, classification, unpack,
interpretation and the copy of the mosaic out of LibRaw, with the encoded bytes already in memory)
on every local authentic sample of every routed mode: 67 samples, one process. Reading the file and
dropping the decoded source are outside the clock and nothing is developed. Each sample is decoded
once by each unpacker first (the first routed unpack parses RawSpeed's camera data, about 4 ms) and
the two mosaics are compared; then 16 observations per unpacker in LibRaw, RawSpeed, RawSpeed,
LibRaw blocks. One-minute load was 3.41 at the start and 4.13 at the end, 2.6 to 4.2 per sample.

| LibRaw decoder | Modes (samples) | LibRaw p50 | RawSpeed p50 | Ratio | Saved per open |
| --- | ---: | ---: | ---: | ---: | ---: |
| `nikon_load_raw()` | 19 (22) | 177–425 ms | 59–161 ms | 2.38–3.30× | 118–264 ms |
| `lossless_jpeg_load_raw()` (CR2) | 7 (7) | 205–315 ms | 56–95 ms | 3.08–3.64× | 149–226 ms |
| `fuji_compressed_load_raw()` (lossless) | 9 (9) | 536–844 ms | 180–304 ms | 2.71–3.02× | 338–541 ms |
| `olympus_load_raw()` | 7 (7) | 258–317 ms | 109–124 ms | 2.28–2.83× | 148–199 ms |
| `pentax_load_raw()` | 2 (2) | 200–223 ms | 59–66 ms | 3.37–3.40× | 141–157 ms |
| `lossless_dng_load_raw()` | 6 (6) | 238–423 ms | 71–151 ms | 2.80–3.37× | 166–272 ms |
| `packed_dng_load_raw()` | 7 (7) | 7.7–292 ms | 3.5–47 ms | 2.16–6.21× | 4–245 ms |
| `panasonic_load_raw()` | 2 (2) | 65.5–66.2 ms | 31.6 ms | 2.07–2.09× | 34–35 ms |
| `panasonicC6_load_raw()` | 1 (1) | 42.9 ms | 28.5 ms | 1.50× | 14 ms |
| `panasonicC8_load_raw()` | 4 (4) | 174–189 ms | 86–97 ms | 1.84–2.22× | 79–104 ms |

Every mode's figure is in [modern camera support](../design/modern-camera-support.md#rawspeed-routed-modes).
The lowest ratio is the Panasonic S5's 1.50×, outside the 1.2–1.4× band that would have been
rerun with more observations; every other mode is 1.84× or more. The spread within a leg is small:
the largest p95 over p50 is 12% (the Leica CL, Q2 and SL2 packed DNGs through RawSpeed, for example Q2 47.0 / 52.2 ms). The uncompressed DJI
DNGs take 8 to 13 ms either way, so their 2.2× saves under 7 ms. The owner Z6 unpacks in
203.6 / 207.4 ms with LibRaw and 65.5 / 66.5 ms with RawSpeed.

### Cold saved-white-balance preparation, LibRaw and RawSpeed

`cold_saved_white_balance_preparation_timing` in `luxforge-core` reproduces the method of
[native development and saved-white-balance preparation](#native-development-and-saved-white-balance-preparation):
a catalog whose RAW has a saved custom red gain of 1.1 × as-shot, a new owner and empty source cache
per observation with the filesystem warm, and the clock from immediately before the photograph's
`source.prepare` (the figures below were taken through `catalog.import` of its file, which queued
the same preparation) until a strict exact-source `PreviewJob` for the current entry is available,
through the one source job's wait and adoption. The harness that produced the earlier figures is
not in the repository, so this one was written to that description. Before is the same source built with the Z6's
`NikonZ6Lossless14` and the Air 2S's `DjiAir2sDng16` on LibRaw (their two `unpacker` lines removed);
after is the source as routed. The two builds differ only in those catalog lines, so each builds
its own catalog. 15 observations per leg in before, after, after, before order give 30 per variant.
The planes of every observation hash the same within and across the two builds.

| Source | Before (LibRaw) p50 / p95 | After (RawSpeed) p50 / p95 | Median time saved | Leg load |
| --- | ---: | ---: | ---: | ---: |
| Nikon Z6 | 303.1 / 305.9 ms | 162.1 / 167.2 ms | 141.0 ms, 46.5% | 4.97–6.65 |
| DJI Air 2S | 205.2 / 207.2 ms | 198.9 / 204.6 ms | 6.3 ms, 3.1% | 3.89–4.97 |
| Fujifilm X100VI, not routed (control) | 364.3 / 368.3 ms | 364.7 / 366.9 ms | none | 3.73–4.75 |

The Z6 saving is the unpack saving above (138 ms), and the X100VI control, whose uncompressed mode
is the same in both builds, moves by 0.4 ms. The Air 2S's packed DNG unpacks in 13 ms either way, so
routing it saves little. The 546.5 / 584.6 ms Z6 figure recorded earlier predates the later
development work in [startup and RAW throughput](#startup-and-raw-throughput); the same
measurement with LibRaw now reads 303.1 / 305.9 ms.

### Peak process RSS

One decode per process under `/usr/bin/time -l` (`one_decode_for_peak_rss`), LibRaw forced and
RawSpeed, five alternating processes each, on the largest routed sensors: the Nikon D850 lossless
NEF (1840, 8288 × 5520, a 91.5 MB mosaic) and the Leica Q2 packed DNG (3204, 8424 × 5632,
94.9 MB). Every run of a variant read within 0.1 MB of the others; the table gives each variant's median.

| Sample (encoded size) | LibRaw peak RSS | RawSpeed peak RSS | Difference |
| --- | ---: | ---: | ---: |
| D850 1840 (55.2 MB) | 242.6 MB | 248.6 MB | +6.0 MB (5.7 MiB) |
| Q2 3204 (87.2 MB) | 281.4 MB | 287.2 MB | +5.9 MB (5.6 MiB) |

The design allowed for a transient second mosaic of about 91 MiB. It does not raise the peak: the
peak is the encoded bytes, LibRaw's raw buffer and the mosaic the adapter keeps, and RawSpeed's
image is released before the adapter allocates that mosaic. What remains is RawSpeed's code and
parsed camera data.

### What these measurements do not claim

The unpack figures are the adapter's decode alone, not an editor open; a first open also reads and
hashes the file, develops, converts and renders. The cold-preparation figures exclude owner
startup, catalog open, rendering and presentation, and cover only the two owner originals that are
routed. RSS is one decode in a test process, not the editor's peak, which also holds developments
and GPU resources. The `timing` tier (`editor-performance`, `editor-latency` and `measure`) opens
generated JPEGs only and does not exercise RAW unpacking; it passed on the measured tree, but its
timing components started at one-minute load 12.0 to 12.6, after the tier's own `check`, so their
rows are marked unreliable and are not compared with earlier figures. Native Windows and Linux
builds are not measured.

## Catalog: browse, pick, develop

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, release builds, 2026-10-01, `cargo run --release --locked --package xtask -- catalog-measure --scale full --raw-corpus <the private CC0 corpus>` at commit 33090901 (30 samples a figure, 5 a first-browse journey), on a host shared with other sessions: one-minute load 13 to 18 throughout, above the harness's threshold of 8, so every figure is marked unreliable by the harness and none is a baseline. The data is generated in scratch: a 1,000-frame RAW trip copied from 102 CC0 files of the corpus under new names with rewritten capture times (the originals read only, checked unchanged), a 10,000-file folder and a 200,000-file tree of APFS clones of generated JPEGs, a 200,000-link tree of hard links, and a generated 100,000-photograph catalog. The OS file cache is warm; no card reader was measured. The photographs-view rows come from the harness's `--only browse-generated` step after the allocation fix (4a8183b5), four runs at load 10 to 23.

| Target (provisional, [catalog design](../design/catalog.md#performance)) | Measured | Verdict |
| --- | --- | --- |
| First browse of a 1,000-frame folder, internal SSD: first grid screen within 1 s; every file, event and moment within 5 s | first screen 181 / 246 ms p50 / p95; every file, event and moment 182 / 249 ms | Met |
| Grid previews for every file of that first browse: reported | 11.7 / 15.1 s (embedded previews of 1,000 RAWs read on two workers; the Canon R5 Mark II and R8 developed) | Reported |
| Browsing from a card reader: reported | Not measured (no card) | Outstanding |
| Returning to a known 1,000-file folder: reconciled and drawn within 1 s | reconciled 4.4 / 4.8 ms, drawn 12.1 / 12.6 ms | Met |
| First index of 200,000 files: reported, the editor responsive | 55.0 s; owner round trips during it 0.025 / 0.41 ms. 200,000 hard links to 500 files: 54.4 s, 0.026 / 0.046 ms | Reported; responsive |
| `browse.view` p95 under 50 ms: 10,000 files | 18.4 / 27.5 ms (first evaluation 66 ms) | Met |
| `browse.view` p95 under 50 ms: 100,000 photographs | 106 / 107 ms at best, 119–167 / 169–188 ms in the other runs, first evaluation about 0.9–1.0 s | **Missed.** Stepping the photographs' rows in SQLite alone takes about 52 ms; the two ways past it are a [proposal](../design/catalog.md#performance) |
| `browse.rows` of 200 under 2 ms | files 2.5 / 3.9 ms, photographs 2.7 / 4.1 ms (0.9 / 1.2 and 1.1 / 1.4 ms in the first run at load 6 to 17) | Missed under this load; met in the earlier run |
| The owner grows by the view's id list | 47–49 MiB settled after the first 100,000-photograph view (the 1.6 MB id list, the preview lane's 20,000 queued tasks, the layout and SQLite's caches); growth over 30 more evaluations 11–15 MiB | Met for held memory |
| Bracket detection from previews under 1 ms a run | 0.033 / 0.058 ms (1,650 runs) | Met |
| Developing 20 RAW picks: reported | 2.69 s; the first photograph committed after 91 ms | Reported |
| Grid scroll over 10,000 files: presented frames p95 within 16 ms at 120 Hz | frame time 8.5 / 9.0 ms (119 frames) | Met |
| Loupe stepping with the look-ahead warm: the next frame presented in the frame after the key | 1 / 1 frames (key to present 0.26 / 0.37 ms); a held arrow at 30 ms presents at 32 / 41 ms intervals; over the RAW trip 1 / 1 frames, held 26 / 58 ms | Met |
| 100% focus check from a full-size embedded preview within 50 ms | 25.6–34.5 ms p50 per camera, 26.6–52.4 ms p95; a second region 25–36 ms | Met at p50; one camera's p95 at 52 ms |
| 100% focus check from an on-demand development: reported per camera | first region 84–1,340 ms p50 by camera (the slowest a large Bayer RAW), a second region of the same frame 25–29 ms | Reported |
| Switching photographs in Develop with a cached large preview: presented in the frame after the key | 1 / 1 frames (31 photographs) | Met |
| Decoded grid and loupe previews within 192 and 256 MiB | grid 32 / 54 MiB, loupe 81 / 87 MiB (high-water mark) | Met |
| Idle CPU with the watchers armed | core 0.30%, editor 0.99% (editor without a catalog view 1.03%) of one core over 30 s | Met |
| A Basic drag during indexing and during a preview backlog | input to presented frame 8.7 / 9.7 ms and 8.7 / 9.4 ms, against 9.0 / 9.6 ms alone (24 MP, warm) | No effect |

## Preview error baseline

What the editor accepted at `46a85159`, measured with the [GPU preview error measure](../design/gpu-preview.md#the-preview-error-limit) so the owner could read the proposed limits against it: the Presence proxy against the exact downscale at Fit, the half-scale 100% motion region against the exact region, and a RAW white-balance draft against its release. The GPU-first stage 5 has since deleted the half-scale region and the refinement after it, and the GPU draws these drags on a machine with one ([below](#a-raw-white-balance-drag-on-the-gpu-against-its-release), [the picture in motion](#the-picture-in-motion)). It is context, not a gate, and it changes no limit. The proposed limits are, per statistic (mean ΔE00, worst 16 × 16 block, p99 ΔE00, signed mean ΔL\*): pointwise 0.5, 1.0, 2.0, ±0.25 and spatial 1.0, 2.5, 5.0, ±0.5.

### Scope

- **Measure.** `cargo xtask preview-error --evidence DIR --candidate-frame N --reference-frame M` (CIEDE2000 in `f64` from 8-bit sRGB through linear light, XYZ and CIELAB under D65, `luxforge_reference::preview_error`) over the photograph rectangle the editor records for each frame, clipped to the canvas, so the canvas beside the photograph never counts. Candidate is the approximate frame, reference the frame that replaces it, and the signed ΔL\* is candidate minus reference. An independent NumPy implementation of the same formulas agreed to six decimals on all four statistics for four of the frame pairs below (`presence-24mp` Dehaze, Z6 100% region, Air 2S Fit white balance, X100VI all three Presence fields); that check is not committed. The report also carried each frame's own recorded state at that build (status label, `render_proxy`, `reduced`, `drawn_region_quality`), which is how each pair below was confirmed to be the frames it names.
- **Host and build.** Apple M4 Pro (14 cores), macOS 26.5.2, Metal on the `Apple M4 Pro` adapter, a hidden 1440 × 900 logical window at 2×, so 2880 × 1800 physical captures. Release build of `luxforge-app` at `46a85159` (this branch changes only the reference crate, xtask and documents), application SHA-256 `916b4b2f76560a9c231c37179bee7a848efd65938fb05e6d6c48370a033dfbff`. Every launch was a hidden window in the background-only bundle. The host was shared (one-minute load 13 to 29 during the launches); these are pixel measurements, not timings, and a loaded host does not change them.
- **Sources.** The generated 24 MP and 60 MP JPEGs (SHA-256 `b54c2a15…` and `b9e0118a…`; four flat colour quadrants, corner labels, an arrow and fine vertical detail, so they hold almost nothing for Presence to act on) and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest (`nikon-z6`, `fujifilm-x100vi`, `dji-air2s`; hashes in [`fixtures/preview/corpus.json`](../../fixtures/preview/corpus.json)). Fit photograph rectangles are 1716 × 1144 (X100VI, Air 2S, 24 MP), 1716 × 1030 (60 MP) and 1004 × 1508 (the Z6, which opens portrait); at 100% it is the whole 1796 × 1660 canvas, the photograph's centre with no pan.
- **One run each.** Each cell is one launch and one drag value, with deterministic pixels but no distribution over photographs, values or pans. Both frames of a pair come from the same launch.

### Presence proxy against the exact downscale at Fit

Candidate: the frame drawn while a Presence slider is dragged to +100 and held, status "Approximate render", the display-bounded proxy. Reference: the frame after the slider is released, status "Exact render" with `settled_from_exact`, the linear-light area reduction of the exact full-resolution render. The editor then displayed an exact-derived Fit frame only for a stack that held a non-neutral Detail layer (Detail declared `fit_settle: exact`; Presence alone kept its proxy as the displayed Fit frame), so every stack below also holds Detail's Colour noise suppression at 1, committed first. The control rows are that Detail layer alone, dragged and released: what settling moves with no Presence at all. It moves nothing on the generated JPEGs and, on the RAWs, the RAW proxy's own difference from the exact reduction (mean 0.41 on the Z6, 0.08 on the X100VI, 0.97 on the Air 2S), which a Presence row includes and cannot be subtracted from. The stack in the last row of each source is Texture and Clarity committed at +100 with Dehaze dragged to +100.

| Fit: Presence proxy against exact reduction | Mean ΔE00 | Worst 16 × 16 block | p99 ΔE00 | Signed mean ΔL\* |
| --- | ---: | ---: | ---: | ---: |
| Z6 NEF, Detail colour 1 alone (control) | 0.406 | 1.84 | 2.01 | +0.024 |
| Z6 NEF, Texture +100 | 0.829 | 3.61 | 4.02 | -0.006 |
| Z6 NEF, Clarity +100 | 0.578 | 2.36 | 2.66 | +0.012 |
| Z6 NEF, Dehaze +100 | 1.768 | 15.13 | 7.46 | -0.083 |
| Z6 NEF, Texture, Clarity, Dehaze +100 | 2.300 | 14.20 | 9.00 | +0.609 |
| X100VI RAF, Detail colour 1 alone (control) | 0.075 | 0.21 | 0.92 | -0.000 |
| X100VI RAF, Texture +100 | 0.481 | 1.73 | 2.01 | -0.018 |
| X100VI RAF, Clarity +100 | 0.305 | 2.80 | 1.17 | -0.017 |
| X100VI RAF, Dehaze +100 | 0.965 | 23.15 | 5.72 | -0.078 |
| X100VI RAF, Texture, Clarity, Dehaze +100 | 1.634 | 25.85 | 7.76 | -0.318 |
| Air 2S DNG, Detail colour 1 alone (control) | 0.974 | 4.31 | 4.36 | +0.079 |
| Air 2S DNG, Texture +100 | 1.847 | 4.92 | 6.03 | -0.090 |
| Air 2S DNG, Clarity +100 | 1.268 | 5.91 | 6.12 | +0.003 |
| Air 2S DNG, Dehaze +100 | 3.625 | 18.62 | 14.36 | -3.167 |
| Air 2S DNG, Texture, Clarity, Dehaze +100 | 4.670 | 15.80 | 14.81 | -2.893 |
| Generated 24 MP JPEG, Detail colour 1 alone (control) | 0.000 | 0.00 | 0.00 | +0.000 |
| Generated 24 MP JPEG, Texture +100 | 0.013 | 1.23 | 0.50 | +0.001 |
| Generated 24 MP JPEG, Clarity +100 | 0.046 | 3.68 | 0.43 | -0.000 |
| Generated 24 MP JPEG, Dehaze +100 | 1.093 | 3.02 | 3.01 | +0.374 |
| Generated 24 MP JPEG, Texture, Clarity, Dehaze +100 | 1.117 | 6.95 | 3.09 | +0.368 |
| Generated 60 MP JPEG, Detail colour 1 alone (control) | 0.000 | 0.00 | 0.00 | +0.000 |
| Generated 60 MP JPEG, Texture +100 | 0.013 | 0.81 | 0.51 | +0.001 |
| Generated 60 MP JPEG, Clarity +100 | 0.050 | 2.39 | 0.47 | -0.001 |
| Generated 60 MP JPEG, Dehaze +100 | 1.285 | 3.74 | 3.57 | +0.317 |
| Generated 60 MP JPEG, Texture, Clarity, Dehaze +100 | 1.312 | 4.05 | 3.76 | +0.322 |

Against the proposed limits, 5 of the 20 Presence rows are within the spatial limits: Texture on the 24 MP, 60 MP and X100VI, and Clarity on the 60 MP and Z6. Dehaze and the three-field stack miss the spatial mean on every source but the X100VI's Dehaze (mean 0.97, but a worst block of 23.1 and a p99 of 5.7), and Dehaze shifts the whole picture: signed ΔL\* +0.3 to +0.4 on the JPEGs and −3.2 on the Air 2S. In the X100VI's worst block of the three-field stack (capture pixel 1082, 470, a dark smooth area) the proxy shows streaks the exact reduction does not.

### Half-scale 100% motion region against the exact region

Candidate: the frame drawn while a Basic Exposure slider is dragged to +1 EV and held at 100% with no pan, `drawn_region_quality` `interactive` (the half-scale visible region), status "Approximate render". Reference: the frame two seconds later, after the shared quiet policy refined the same open draft to full detail, status "Exact render". Exposure is exact on a RAW photograph, so the pair isolates the half-scale softness. The centre of both generated JPEGs at 100% is a flat red field, so their rows are exactly zero and say nothing about softness; the RAW rows are the figures.

| 100%: half-scale motion region against exact region | Mean ΔE00 | Worst 16 × 16 block | p99 ΔE00 | Signed mean ΔL\* |
| --- | ---: | ---: | ---: | ---: |
| Z6 NEF | 0.485 | 3.44 | 1.76 | +0.008 |
| X100VI RAF | 0.759 | 1.44 | 2.44 | +0.005 |
| Air 2S DNG | 2.761 | 9.31 | 12.63 | +0.325 |
| Generated 24 MP JPEG | 0.000 | 0.00 | 0.00 | +0.000 |
| Generated 60 MP JPEG | 0.000 | 0.00 | 0.00 | +0.000 |

The floating toolbar at the canvas's foot and the scroll bars are inside the 100% rectangle and identical in both frames, so they add zeros: about 3% of its pixels, which dilutes the means.

### RAW white-balance draft against its release

The `raw-panel` smoke scenario's frames: a Temperature drag left open at 3500 K at Fit, and again at 2500 K at 100%, each against its release at the same value, which redevelops the mosaic. The draft is the approximation `W = R · diag(g'/g) · R⁻¹` on the planes developed at the committed white balance; the release is the exact development. At Fit the pair is the draft (frame 2) and the release's proxy (frame 3). At 100% the moving frame (frame 5: the half-scale region with the approximate white balance, so softness and white balance together) and the held draft after full-detail refinement (frame 6) are each held against the release's exact region (frame 7).

| RAW white balance, Temperature drag | Mean ΔE00 | Worst 16 × 16 block | p99 ΔE00 | Signed mean ΔL\* |
| --- | ---: | ---: | ---: | ---: |
| Z6 NEF, Fit: draft against release | 0.038 | 0.38 | 0.74 | -0.000 |
| Z6 NEF, 100%: held refined draft against release | 0.062 | 0.44 | 0.48 | -0.000 |
| Z6 NEF, 100%: moving frame against release | 0.215 | 2.31 | 1.17 | +0.009 |
| X100VI RAF, Fit: draft against release | 0.059 | 0.24 | 0.66 | +0.000 |
| X100VI RAF, 100%: held refined draft against release | 0.142 | 0.45 | 0.73 | -0.003 |
| X100VI RAF, 100%: moving frame against release | 0.416 | 0.96 | 1.35 | +0.003 |
| Air 2S DNG, Fit: draft against release | 0.313 | 2.68 | 1.84 | -0.030 |
| Air 2S DNG, 100%: held refined draft against release | 0.408 | 3.11 | 3.08 | -0.068 |
| Air 2S DNG, 100%: moving frame against release | 1.641 | 8.24 | 10.01 | +0.144 |

Against the proposed pointwise limits, the Z6 and X100VI drafts are within them at Fit and when held at 100% (the Z6's moving frame misses only the worst block, 2.31 against 1.0), and the Air 2S misses the worst block at Fit (2.68) and the worst block and p99 when held at 100%.

### A RAW white-balance drag on the GPU against its release

Since 2026-10-06 the GPU draws a Temperature drag over the planes the entry developed, the change `W` a leading step ([GPU previews](../design/gpu-preview.md#raw-white-balance)), and the owner's `raw-panel` gates hold that moving frame to the release's picture at rest within the pointwise limits, at Fit and over the visible region at 100%. Measured by `gpu_white_balance_on_the_raw_corpus` (`crates/luxforge-app/src/app/gpu_white_balance_tests.rs`, ignored; the private RAW manifest), on the M4 Pro's Metal adapter, built from the lane's `c22847be` plus the test: each RAW opened As shot, a `set-raw` draft at 3500 K and at 2500 K planned at Fit (the harness's Fit bounds) and over the largest view's centred region at 100%, drawn by the editor's surface over its boundary derived from the As shot source; then the same temperature committed, redeveloped, and its picture at rest drawn from the redeveloped source (its tiles reduced to the view at Fit, its region plan at 100%) and its view plan, the frame a drag over the redeveloped source draws. Figures are mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / signed mean ΔL\*; one draw each, deterministic.

| Source, view, temperature | Moving frame against the picture at rest (the gate) | Moving frame against the redeveloped view plan (`W` alone) | View plan against the picture at rest (no change at all) |
| --- | --- | --- | --- |
| Z6, Fit, 3500 K | 0.385 / 2.11 / 2.11 / +0.025 | 0.038 / 0.34 / 0.74 / −0.000 | 0.384 / 2.10 / 2.11 / +0.025 |
| Z6, Fit, 2500 K | 0.288 / 1.89 / 1.85 / +0.022 | 0.060 / 0.42 / 0.74 / −0.001 | 0.282 / 1.88 / 1.83 / +0.023 |
| Z6, 100%, 3500 K | 0.151 / 2.33 / 1.26 / +0.000 | the same | 0 |
| Z6, 100%, 2500 K | 0.243 / 2.25 / 1.57 / −0.000 | the same | 0 |
| X100VI, Fit, 3500 K | 0.098 / 0.38 / 0.77 / −0.001 | 0.059 / 0.24 / 0.66 / +0.000 | 0.072 / 0.21 / 0.71 / −0.001 |
| X100VI, Fit, 2500 K | 0.127 / 0.50 / 0.80 / −0.004 | 0.111 / 0.50 / 0.77 / −0.000 | 0.049 / 0.25 / 0.59 / −0.004 |
| X100VI, 100%, 3500 K | 0.205 / 1.16 / 1.21 / +0.002 | the same | 0 |
| X100VI, 100%, 2500 K | 0.632 / 2.33 / 2.49 / +0.003 | the same | 0 |
| Air 2S, Fit, 3500 K | 1.086 / 5.62 / 5.54 / +0.069 | 0.317 / 2.71 / 1.91 / −0.029 | 1.027 / 4.70 / 5.07 / +0.098 |
| Air 2S, Fit, 2500 K | 0.913 / 6.34 / 5.82 / −0.025 | 0.374 / 4.99 / 3.13 / −0.107 | 0.817 / 4.66 / 4.87 / +0.082 |
| Air 2S, 100%, 3500 K | 0.675 / 4.89 / 3.45 / −0.024 | the same | 0 |
| Air 2S, 100%, 2500 K | 0.794 / 8.75 / 5.25 / −0.122 | the same | 0 |

- **Within the pointwise limits:** the X100VI at Fit at both temperatures. Every other cell misses at least the worst block; none is excused, since none of the three scenes is a highlight-clipped Bayer one by the owner's measure, the Z6's clipped share 1.5 × 10⁻⁶.
- **At Fit the miss is the drag's frame, not `W`.** The moving frame against the redeveloped view plan, `W` alone, is within the limits on the Z6 and X100VI (worst block at most 0.50) and the Air 2S misses as its CPU draft did (2.71 and 4.99 against the CPU's 2.68 at 3500 K). The view plan against the picture at rest, the same stack with nothing changed, already misses on the Z6 (worst block 2.10 and 1.88) and the Air 2S (4.70 and 4.66), whose lens profiles warp the stage: a drag's frame at Fit processes the source reduced to the view and the picture at rest reduces the processed stage, the difference the release gate reports on 462 of 783 cells and the owner's open question of what a drag's frame at Fit is held to.
- **At 100% the miss is `W`.** The region is the picture at rest's own plan, so the figures are `W`'s alone: over the largest view's centred region the Z6 and X100VI miss the worst block (2.25 to 2.33) and the X100VI at 2500 K the p99 too (2.49), where the CPU's held full-detail draft over the 1440 × 900 window's region missed nothing ([above](#raw-white-balance-draft-against-its-release)); the first-order `diag(g'/g)` does not follow the demosaic at high-contrast edges and saturated colours.

`raw-panel` itself failed on all three RAWs at its `crop-started` step ("the luxforge.basic section records false, expected expanded"), after every frame used here had passed its plan checks and before the scenario's own residual checks ran. Nothing here uses those checks, and the owner's relative-residual gates (held 100% residuals of 0.8%, 1.4% and 5.1%, [decisions](../decisions.md#raw-white-balance-drafts)) were not remeasured.

### Reproducing it

The Presence journey is a source-independent evidence script, [`fixtures/preview/baseline/presence-fit.json`](../../fixtures/preview/baseline/presence-fit.json). Frame 1 is the open and each step is the frame after it. The Presence pairs (candidate, reference) were the Detail control (2, 3), Texture (4, 5), Clarity (8, 9), Dehaze (12, 13) and all three (17, 18). The region pair was frames 3 and 4 of a journey that zoomed to 100% and held an exposure drag for 2 s before releasing it; its script went with the half-scale region it compared. The white-balance pairs came from the `raw-panel` smoke scenario's evidence: Fit (2, 3), 100% moving (5, 7) and held (6, 7).

Run today, the Presence journey draws the GPU's frames, not the CPU proxy, so it no longer reproduces these pairs:

```sh
cargo xtask build --release
cargo xtask develop --background --hidden-window --evidence-dir /tmp/NEW_DIR \
  --evidence-script fixtures/preview/baseline/presence-fit.json --open SOURCE --window-size 1440 900
cargo xtask preview-error --evidence /tmp/NEW_DIR --candidate-frame 4 --reference-frame 5
```

The comparison under today's contract is the release gate's picture in motion, a drag's frame against the picture at rest it settles to and against the reference renderer's frame at the view, not against a CPU frame (a selection, so the run reports itself incomplete):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind picture-in-motion --families presence --zoom fit
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind picture-in-motion --families basic --zoom 100
```

## GPU colour programs at Fit

The seven pointwise colour programs ([GPU previews](../design/gpu-preview.md#qualifying-a-program)) and the geometry tail ([GPU previews](../design/gpu-preview.md#where-the-code-lives)) against the CPU frame each previewed at Fit on that build, on every colour and geometry recipe of the [corpus](../../fixtures/preview/corpus.json) (`basic`, `tone-curve`, `mixer`, `vignette` and the four together, `colour-stack`; a 16:9 crop straightened by 7° and by 45° and a tight crop straightened by 7°, a Perspective warp and, on the RAWs and on the zone plate with a lens identity, their lens profile with it, each under a Basic edit) over every source this host has. Pixel measurements, not timings. That CPU frame was the preview worker's proxy, which the GPU-first stage 5 deleted, so these figures record the programs' qualification, not the editor's frames now.

### Scope

- **Measure.** `cargo xtask preview-error --candidate GPU.png --reference CPU.png --photo-rect 0,0,W,H --class pointwise` over each pair; all 62 measured pairs pass. Candidate is the GPU frame, reference the CPU frame; the photograph is the whole frame.
- **The frames.** The CPU frame was the desktop's own Fit job (`ready_preview_job` with the Fit bounds) rendered by the preview worker's proxy phase, or, for a photograph that fits the bounds at its own size, its exact phase. The GPU frame was the same stack planned with `gpu_plan` over the proxy the worker built — a window of the proxy stage when the stack reads less than all of it, as a crop does, held at its origin — as the boundary the worker's job writes, or for a frame at the exact stage the window of the source its whole output reads, as the worker renders it (`qualification::region_boundary` over the whole output, `Render::output_boundary`) (`rgba16float` on a JPEG, `rgba32float` on a RAW), converted by `app::gpu_plan` with a lens warp's coordinate grid over the output stage (a perspective's homography needs none) and drawn by the photo surface's own assembled shader, whose last pass encodes as the CPU's output quantizer does, read back headlessly. Both are compared at the stage's own size, before the surface's placement draws them on screen; [TASK-002's readback](../design/gpu-preview.md#where-the-code-lives) showed the two draw identically for identical codes.
- **Bounds.** 1716 × 1508 physical pixels: an evidence run's 1440 × 900 logical window at 2× with both panels open, as `proxy_bounds_for` computes them.
- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The `test` profile build of `luxforge-app` at `61592997`, `gpu_colour_corpus_at_fit`. One run; the pixels are deterministic.
- **Sources.** The generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture (SHA-256 `b54c2a15…`, `b9e0118a…`, `dfae0f2d…`, `65de25c7…`, `2491b2d0…`, matching the corpus) and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest. The Presence fixture (1440 × 960) fits the bounds, so its Fit frame is the exact render.
- **RAW lens profiles.** The Z6 and Air 2S commit their lens profile at first open, a warp after the colour layers, so each of their cells draws the geometry tail through the profile's coordinate grid; the zone plate with a lens identity has its detected profile applied as the Lens section's Apply applies it. The vignette alone on those two is a gap: a finishing layer after a warp receives the warped frame, which only the worker's boundary job holds, and the harness holds the proxy source. The X100VI commits no profile.
- **Straightened crops.** A crop behind the Z6's and Air 2S's lens profile is fused into the warp's resample, and its Fit frame was a windowed proxy as on every other source, whose boundary holds only the window of the proxy stage the crop reads: at 7° 1781 × 1143 and 1819 × 1171 `f32` texels (32.6 and 34.1 MB) of 1821 × 2738 and 1821 × 1213, and at 45° 1869 × 1869 and 1899 × 1896 (55.9 and 57.6 MB), each slot charging 65.7 to 103.2 MB. The tight crop's output fits the bounds on every source but the 60 MP JPEG, so its Fit frame is the exact phase's, over the window of the source its output reads: 969 × 1343 texels of the Z6's 4024 × 6048 (20.8 MB, where the whole source is 389 MB), 1173 × 998 of the Air 2S's 5464 × 3640 (18.7 of 318 MB) and 1636 × 1389 of the X100VI's 7728 × 5152 (36.4 of 637 MB), each slot 44.9 to 71.3 MB.

### Results

Mean ΔE00, worst 16 × 16 block, p99, signed mean ΔL\* and largest ΔE00 of the drawn frame, and what the slot drawing it charges the GPU-preview budget. The drawn frame and the program's `f32` output through the reference quantizer are the same in every cell: the last pass encodes as the CPU's quantizer does, where the hardware's sRGB encoder rounded some values to the neighbouring code (it put the RAW cells' means at 0.04 to 0.12 against 0.01 to 0.04 for the program output).

| Recipe, source | Stage | Mean | Worst block | p99 | Signed ΔL\* | Max | Charged |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Basic, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.21 | 32.5 MB |
| Basic, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.00 | 0.00 | +0.000 | 0.12 | 30.9 MB |
| Basic, zone plate | 1716 × 1144 | 0.006 | 0.06 | 0.00 | +0.000 | 0.84 | 32.5 MB |
| Basic, Presence fixture | 1440 × 960, exact | 0.010 | 0.16 | 0.68 | −0.002 | 0.84 | 27.8 MB |
| Basic (RAW), Z6 | 1003 × 1508 | 0.015 | 0.07 | 0.59 | −0.000 | 1.76 | 65.2 MB |
| Basic (RAW), X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.89 | 48.2 MB |
| Basic (RAW), Air 2S | 1716 × 1143 | 0.017 | 0.10 | 0.58 | −0.000 | 1.63 | 79.7 MB |
| Tone curve, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.00 | 0.00 | −0.000 | 0.34 | 32.5 MB |
| Tone curve, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.01 | 0.00 | −0.000 | 0.30 | 30.9 MB |
| Tone curve, zone plate | 1716 × 1144 | 0.001 | 0.03 | 0.00 | −0.002 | 0.39 | 32.5 MB |
| Tone curve, Presence fixture | 1440 × 960, exact | 0.001 | 0.07 | 0.00 | −0.001 | 0.39 | 27.8 MB |
| Tone curve, Z6 | 1003 × 1508 | 0.016 | 0.09 | 0.68 | −0.000 | 1.45 | 65.2 MB |
| Tone curve, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 1.04 | 48.2 MB |
| Tone curve, Air 2S | 1716 × 1143 | 0.019 | 0.13 | 0.69 | −0.000 | 1.71 | 79.7 MB |
| Mixer, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.02 | 0.00 | +0.000 | 0.34 | 32.5 MB |
| Mixer, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.00 | 0.00 | −0.000 | 0.09 | 30.9 MB |
| Mixer, zone plate | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.00 | 32.5 MB |
| Mixer, Presence fixture | 1440 × 960, exact | 0.000 | 0.00 | 0.00 | +0.000 | 0.00 | 27.8 MB |
| Mixer, Z6 | 1003 × 1508 | 0.019 | 0.09 | 0.69 | −0.000 | 1.47 | 65.2 MB |
| Mixer, X100VI | 1716 × 1144 | 0.000 | 0.01 | 0.00 | +0.000 | 1.03 | 48.2 MB |
| Mixer, Air 2S | 1716 × 1143 | 0.019 | 0.12 | 0.67 | −0.000 | 1.66 | 79.7 MB |
| Vignette, 24 MP JPEG | 1716 × 1144 | 0.004 | 0.04 | 0.21 | +0.001 | 0.77 | 32.5 MB |
| Vignette, 60 MP JPEG | 1716 × 1030 | 0.004 | 0.03 | 0.21 | +0.001 | 0.66 | 30.9 MB |
| Vignette, zone plate | 1716 × 1144 | 0.003 | 0.02 | 0.00 | +0.001 | 0.40 | 32.5 MB |
| Vignette, Presence fixture | 1440 × 960, exact | 0.002 | 0.02 | 0.00 | −0.000 | 0.40 | 27.8 MB |
| Vignette, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | −0.000 | 0.76 | 48.2 MB |
| Colour stack, 24 MP JPEG | 1716 × 1144 | 0.004 | 0.04 | 0.21 | +0.001 | 0.62 | 32.5 MB |
| Colour stack, 60 MP JPEG | 1716 × 1030 | 0.004 | 0.03 | 0.21 | +0.001 | 0.62 | 30.9 MB |
| Colour stack, zone plate | 1716 × 1144 | 0.015 | 0.07 | 0.57 | +0.001 | 1.10 | 32.5 MB |
| Colour stack, Presence fixture | 1440 × 960, exact | 0.013 | 0.25 | 0.57 | −0.000 | 1.16 | 27.8 MB |
| Colour stack (RAW), Z6 | 1003 × 1508 | 0.018 | 0.09 | 0.68 | −0.000 | 1.83 | 65.2 MB |
| Colour stack (RAW), X100VI | 1716 × 1144 | 0.000 | 0.01 | 0.00 | +0.000 | 1.07 | 48.2 MB |
| Colour stack (RAW), Air 2S | 1716 × 1143 | 0.017 | 0.12 | 0.63 | +0.000 | 1.71 | 79.7 MB |
| Straightened crop, 24 MP JPEG | 1715 × 964 | 0.000 | 0.00 | 0.00 | −0.000 | 0.80 | 42.4 MB |
| Straightened crop, 60 MP JPEG | 1715 × 964 | 0.000 | 0.00 | 0.00 | +0.000 | 0.49 | 42.3 MB |
| Straightened crop, zone plate | 1715 × 964 | 0.002 | 0.02 | 0.00 | −0.002 | 0.40 | 42.4 MB |
| Straightened crop, Presence fixture | 1356 × 762, exact | 0.000 | 0.06 | 0.00 | −0.000 | 0.39 | 32.8 MB |
| Straightened crop, Z6 | 1715 × 964 | 0.019 | 0.11 | 0.65 | −0.000 | 1.59 | 82.0 MB |
| Straightened crop, X100VI | 1715 × 964 | 0.000 | 0.00 | 0.00 | +0.000 | 0.95 | 85.0 MB |
| Straightened crop, Air 2S | 1715 × 964 | 0.031 | 0.16 | 0.76 | −0.000 | 1.55 | 85.0 MB |
| Crop at 45°, 24 MP JPEG | 1716 × 965 | 0.000 | 0.00 | 0.00 | +0.000 | 0.00 | 60.0 MB |
| Crop at 45°, 60 MP JPEG | 1715 × 964 | 0.000 | 0.00 | 0.00 | +0.000 | 0.00 | 60.0 MB |
| Crop at 45°, zone plate | 1716 × 965 | 0.003 | 0.02 | 0.00 | −0.002 | 0.40 | 60.0 MB |
| Crop at 45°, Presence fixture | 867 × 487, exact | 0.000 | 0.04 | 0.00 | −0.000 | 0.35 | 15.3 MB |
| Crop at 45°, Z6 | 1715 × 964 | 0.019 | 0.12 | 0.65 | +0.000 | 1.64 | 128.6 MB |
| Crop at 45°, X100VI | 1715 × 964 | 0.000 | 0.01 | 0.00 | +0.000 | 1.06 | 132.0 MB |
| Crop at 45°, Air 2S | 1715 × 964 | 0.051 | 0.20 | 0.87 | +0.000 | 1.69 | 132.0 MB |
| Tight straightened crop, 24 MP JPEG | 1160 × 940, exact | 0.000 | 0.00 | 0.00 | +0.000 | 0.79 | 33.2 MB |
| Tight straightened crop, 60 MP JPEG | 1716 × 1284 | 0.000 | 0.01 | 0.00 | −0.000 | 0.77 | 50.1 MB |
| Tight straightened crop, zone plate | 1160 × 940, exact | 0.001 | 0.01 | 0.00 | −0.001 | 0.40 | 33.2 MB |
| Tight straightened crop, Presence fixture | 278 × 226, exact | 0.000 | 0.00 | 0.00 | +0.000 | 0.32 | 2.0 MB |
| Tight straightened crop, Z6 | 852 × 1299, exact | 0.015 | 0.14 | 0.63 | +0.000 | 1.53 | 58.4 MB |
| Tight straightened crop, X100VI | 1494 × 1211, exact | 0.000 | 0.01 | 0.00 | +0.000 | 1.13 | 89.5 MB |
| Tight straightened crop, Air 2S | 1056 × 856, exact | 0.042 | 0.15 | 0.85 | −0.000 | 1.61 | 54.2 MB |
| Perspective warp, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.01 | 0.00 | +0.000 | 0.41 | 39.0 MB |
| Perspective warp, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.01 | 0.00 | −0.000 | 0.37 | 36.8 MB |
| Perspective warp, zone plate | 1716 × 1144 | 0.002 | 0.04 | 0.00 | −0.002 | 0.40 | 39.0 MB |
| Perspective warp, Presence fixture | 1440 × 960, exact | 0.000 | 0.07 | 0.00 | −0.001 | 0.39 | 32.4 MB |
| Lens and perspective warp, Z6 | 1003 × 1508 | 0.011 | 0.10 | 0.58 | −0.000 | 1.52 | 62.8 MB |
| Lens and perspective warp, X100VI | 1716 × 1144 | 0.000 | 0.01 | 0.00 | +0.000 | 1.05 | 76.0 MB |
| Lens and perspective warp, Air 2S | 1716 × 1143 | 0.024 | 0.11 | 0.69 | −0.000 | 1.64 | 75.9 MB |
| Lens and perspective warp, zone plate with a lens identity | 1716 × 1142 | 0.023 | 0.17 | 0.34 | −0.002 | 1.11 | 38.5 MB |

Every measured cell is within the pointwise limits (mean 0.5, worst block 1.0, p99 2.0, ΔL\* ±0.25): the largest figures are a mean of 0.051, a worst block of 0.25, a p99 of 0.87 and a ΔL\* of −0.002. Without a lens warp the GPU frame is the CPU's to within a fraction of a code: the JPEGs, whose boundary is each 8-bit code's linear value held as a half float, and the X100VI, whose `f32` boundary holds what the CPU reads. A perspective warp's tail is its homography, evaluated at every pixel, so it keeps that: over the zone plate, whose chirp reaches half a cycle a pixel, its worst block is 0.04. The Z6 and Air 2S cells and the zone plate with a lens identity carry a lens profile's warp, which the tail samples through its coordinate grid, within 0.025 px of the exact map the CPU samples through; their means are 0.01 to 0.05 and their worst blocks 0.07 to 0.20, the largest the Air 2S's crop at 45°. The RAW tail's `rgba32float` intermediate keeps the linear values unquantized, and the RAW cells carry a signed ΔL\* within ±0.0001, where an `rgba16float` one written by the M4's own conversion toward zero carried −0.004 to −0.010 ([plane precision](#plane-precision)). A windowed boundary changes nothing the tail draws: on the device, the frame over the window a crop reads is the whole boundary's, bit for bit, through an affine, a projective and a lens tail, and with Texture and Clarity before the tail over the window grown by their margin (`cargo test -p luxforge-app gpu_window`).

### Reproducing it

The harness that measured these figures, `gpu_colour_corpus_at_fit`, is deleted with the CPU proxy it compared against. The release gate runs the same families at Fit against the reference renderer's frame reduced to the view, not against the CPU proxy, so it reproduces the comparison under today's contract, not these figures. The picture in motion is the drag's frame these figures measured, now held against the picture at rest and the reference; the picture at rest is what the gate judges. A selection, so the run reports itself incomplete:

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families basic,tone-curve,mixer,vignette,colour-stack,crop,lens-perspective
```

## GPU mask coverage at Fit

The coverage programs of every mask kind ([GPU previews](../design/gpu-preview.md#mask-coverage)), each carrying the corpus's masked Basic layer (Exposure +0.8, Contrast 20, Vibrance 30), against the CPU frame each previewed at Fit on that build, the preview worker's proxy, since deleted, on every masked recipe of the [corpus](../../fixtures/preview/corpus.json) (`mask-linear`, `mask-radial`, `mask-brush`, `mask-luminance-range`, `mask-colour-range` and the algebra, `mask-composed`, a linear less a radial) over every source this host has. These are the figures the programs were enabled on. Pixel measurements, not timings.

### Scope

- **Measure.** `cargo xtask preview-error --candidate GPU.png --reference CPU.png --photo-rect 0,0,W,H --class pointwise` over each pair; all 42 measured pairs pass, the zone plate's six reported in [the qualification](#the-corpus-error-report).
- **The frames.** As for [the colour programs](#gpu-colour-programs-at-fit): the desktop's own Fit job through the preview worker against the same stack planned with `gpu_plan` over the same proxy source held as the worker's boundary holds it, converted by `app::gpu_plan` into one masked step, and the lens profile's tail on the Z6 and Air 2S, and drawn by the photo surface's own assembled shader, read back headlessly. The corpus names its mask and components by name; the harness resolves each to its identity, and plans from the recipe the frame was rendered from, whose painted strokes are resolved.
- **Bounds, host and build.** 1716 × 1508 physical pixels. Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The `test` profile build of `luxforge-app` at `61592997`, `gpu_mask_corpus_at_fit`. One run; the pixels are deterministic.
- **Sources and gaps.** The generated 24 MP and 60 MP JPEGs and the Presence fixture, and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest, the Z6 and Air 2S with the lens profile their first open commits. [the qualification](#the-corpus-error-report) measures the zone plate.
- **An empty mask is a gap.** A cell whose mask selects nothing on its source (read back over the boundary before the frame) is recorded as a gap, not a pass. No measured cell was empty. On the generated JPEGs the colour range covers some pixels, and its GPU and CPU frames are identical code for code.

### Results

Mean ΔE00, worst 16 × 16 block, p99, signed mean ΔL\* and largest ΔE00 of the drawn frame, and the slot's charge. As for the colour programs, the drawn frame is the program's output through the reference quantizer in every cell.

| Recipe, source | Stage | Mean | Worst block | p99 | Signed ΔL\* | Max | Charged |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Linear, 24 MP JPEG | 1716 × 1144 | 0.003 | 0.02 | 0.12 | +0.001 | 0.41 | 32.5 MB |
| Linear, 60 MP JPEG | 1716 × 1030 | 0.003 | 0.03 | 0.22 | +0.001 | 0.34 | 30.9 MB |
| Linear, Presence fixture | 1440 × 960, exact | 0.004 | 0.03 | 0.28 | +0.004 | 1.00 | 27.8 MB |
| Linear, Z6 | 1003 × 1508 | 0.018 | 0.11 | 0.64 | −0.000 | 1.67 | 65.2 MB |
| Linear, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.96 | 48.2 MB |
| Linear, Air 2S | 1716 × 1143 | 0.020 | 0.11 | 0.64 | +0.000 | 1.63 | 79.7 MB |
| Radial, 24 MP JPEG | 1716 × 1144 | 0.001 | 0.04 | 0.00 | +0.000 | 0.63 | 32.5 MB |
| Radial, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.03 | 0.00 | +0.000 | 0.38 | 30.9 MB |
| Radial, Presence fixture | 1440 × 960, exact | 0.000 | 0.03 | 0.00 | +0.000 | 0.93 | 27.8 MB |
| Radial, Z6 | 1003 × 1508 | 0.016 | 0.09 | 0.65 | −0.000 | 1.47 | 65.2 MB |
| Radial, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.87 | 48.2 MB |
| Radial, Air 2S | 1716 × 1143 | 0.018 | 0.11 | 0.66 | +0.000 | 1.71 | 79.7 MB |
| Brush, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.02 | 0.00 | +0.000 | 0.42 | 32.5 MB |
| Brush, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.03 | 0.00 | +0.000 | 0.36 | 30.9 MB |
| Brush, Presence fixture | 1440 × 960, exact | 0.001 | 0.07 | 0.00 | +0.001 | 0.91 | 27.8 MB |
| Brush, Z6 | 1003 × 1508 | 0.016 | 0.09 | 0.65 | −0.000 | 1.46 | 65.2 MB |
| Brush, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.52 | 48.2 MB |
| Brush, Air 2S | 1716 × 1143 | 0.019 | 0.11 | 0.66 | +0.000 | 1.71 | 79.7 MB |
| Luminance range, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.02 | 0.00 | +0.000 | 0.28 | 32.5 MB |
| Luminance range, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.00 | 0.00 | −0.000 | 0.13 | 30.9 MB |
| Luminance range, Presence fixture | 1440 × 960, exact | 0.008 | 0.10 | 0.28 | +0.009 | 0.39 | 27.8 MB |
| Luminance range, Z6 | 1003 × 1508 | 0.020 | 0.11 | 0.66 | +0.000 | 1.52 | 65.2 MB |
| Luminance range, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.96 | 48.2 MB |
| Luminance range, Air 2S | 1716 × 1143 | 0.021 | 0.12 | 0.64 | +0.000 | 1.53 | 79.7 MB |
| Colour range, 24 MP JPEG | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.00 | 32.5 MB |
| Colour range, 60 MP JPEG | 1716 × 1030 | 0.000 | 0.00 | 0.00 | +0.000 | 0.00 | 30.9 MB |
| Colour range, Presence fixture | 1440 × 960, exact | 0.008 | 0.10 | 0.28 | +0.009 | 0.39 | 27.8 MB |
| Colour range, Z6 | 1003 × 1508 | 0.018 | 0.11 | 0.64 | −0.000 | 1.67 | 65.2 MB |
| Colour range, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 1.09 | 48.2 MB |
| Colour range, Air 2S | 1716 × 1143 | 0.021 | 0.12 | 0.65 | +0.000 | 1.61 | 79.7 MB |
| Linear less radial, 24 MP JPEG | 1716 × 1144 | 0.003 | 0.02 | 0.07 | +0.001 | 0.43 | 32.5 MB |
| Linear less radial, 60 MP JPEG | 1716 × 1030 | 0.003 | 0.03 | 0.22 | +0.001 | 0.43 | 30.9 MB |
| Linear less radial, Presence fixture | 1440 × 960, exact | 0.004 | 0.03 | 0.28 | +0.004 | 1.00 | 27.8 MB |
| Linear less radial, Z6 | 1003 × 1508 | 0.018 | 0.11 | 0.64 | +0.000 | 1.67 | 65.2 MB |
| Linear less radial, X100VI | 1716 × 1144 | 0.000 | 0.00 | 0.00 | +0.000 | 0.96 | 48.2 MB |
| Linear less radial, Air 2S | 1716 × 1143 | 0.020 | 0.11 | 0.64 | +0.000 | 1.63 | 79.7 MB |

Every measured cell is within the pointwise limits (mean 0.5, worst block 1.0, p99 2.0, ΔL\* ±0.25): the largest figures are a mean of 0.021, a worst block of 0.12, a p99 of 0.66 and a ΔL\* of +0.009. They are the colour programs' own figures for the same Basic layer: on the JPEGs and the X100VI the GPU frame is the CPU's to within a fraction of a code, and on the Z6 and Air 2S the lens profile's warp dominates as it does there, the RAW tail's `rgba32float` intermediate holding their signed ΔL\* within ±0.0001 ([plane precision](#plane-precision)). The masks' coverage itself is held to the CPU's by the contour rule in the readback tests (`cargo test -p luxforge-app gpu_mask`), within 0.004 px on every case.

### Reproducing it

The harness that measured these figures, `gpu_mask_corpus_at_fit`, is deleted with the CPU proxy it compared against. The release gate runs the mask families at Fit against the reference renderer's frame, not the CPU proxy, so it reproduces the comparison under today's contract, not these figures, as [for the colour programs](#gpu-colour-programs-at-fit):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families mask-linear,mask-radial,mask-brush,mask-luminance-range,mask-colour-range,mask-composed
```

## GPU Presence program

The Presence program ([GPU previews](../design/gpu-preview.md#spatial-programs)) on the M4, against the CPU per filter, per unit and on the [corpus](../../fixtures/preview/corpus.json)'s Presence recipes at Fit. These are the figures the program was enabled on. Pixel and arithmetic measurements, not timings.

### Scope

- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal (`wgpu-hal` 27.0.4, Metal's default fast math). The `test` profile build of `luxforge-app` at `618d02eb` for the per-filter and per-unit figures, and at `61592997` for the corpus. One run each; the pixels are deterministic.
- **The readback.** Every GPU figure is the photo surface's own spatial step: its generated pass modules and frame module, run headlessly by the surface's qualification readback ([qualifying a program](../design/gpu-preview.md#qualifying-a-program)).
- **The CPU.** Per filter, the production filters of `modules/presence/filters.rs` over a whole frame (`luxforge_core::qualification`, test builds only). Per unit and on the corpus, the CPU frame of the same stack.

### Per filter

Each kernel through the surface's passes over a synthetic plane (a ramp, a step edge, noise and a flat patch, held as half floats as the boundary holds them) against the CPU filter it transcribes, which accumulates its box sums in `f64`. The largest absolute difference:

| Filter | Cases | Largest difference |
| --- | --- | ---: |
| Box mean, two passes | radii 1, 2, 6, 14, 24, 38 × runs 1, 4, 16, 64, values in [−0.25, 1.5] | 1.1 × 10⁻⁶ |
| Box minimum, two passes | radii 1, 2, 3, 5 | 0 (exact) |
| `1 − ω · min`, the raw transmission | ω = 0.8 | 6.0 × 10⁻⁸, one rounding: Metal fuses the multiply-add |
| Self-guided filter | Texture's ε at radii 1, 2, 6; Clarity's at 6, 24, 38; runs 1 and 16 | 4.4 × 10⁻⁶ |
| Guided filter | Dehaze's ε (10⁻⁴) at radii 1, 3, 6, 10; runs 1 and 16 | 2.0 × 10⁻⁵ |
| 4× reduction of the encoded luminance | 203 × 131, partial last blocks | 2.4 × 10⁻⁷ |
| 16× reduction of the colour | the same | 1.2 × 10⁻⁷ |
| Bilinear upsample by 4 | the same | 1.5 × 10⁻⁸ |
| Soft clip | excursions in [−4, 4], encoded values in [−0.2, 1.2], both units' limits | 1.5 × 10⁻⁸; no zero moved |
| Atmospheric light | 640 × 480 (the minimum count), 2600 × 1700 (17,441 blocks, 18 selected), a tied bright plateau | 0: the CPU's light narrowed to `f32` |

The run length of the box means' running sums does not change their precision measurably: every run from 1 (a direct sum per output) to 64 stays within 1.1 × 10⁻⁶ at every radius. The program runs 16.

### Per unit

Every Presence combination at ±100 (each field alone, Texture and Clarity, all three) and one mixed case, unmasked and masked by a feathered radial, over a synthetic photograph (a hazy sky over darker ground, a hard horizon, fine texture and noise) at 480 × 320 and 1536 × 1024 on the linear path, against the CPU frame of the same stack; Dehaze both with the light the CPU frame stored and with the one the GPU takes from the stage it holds. 64 cases, 32 masked. Worst of each statistic:

| Measured | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Program output through the reference quantizer | 0.0007 | 0.009 | 0.00 | +0.0000 | 1.22 |
| Codes the hardware encoder draws | 0.030 | 0.18 | 0.76 | −0.0030 | 1.71 |
| Byte path, 960 × 640, all three at +100 and −100, light stored | 0.093 | 0.69 | 0.76 | −0.0008 | 34.9 |

No output is non-finite. Dehaze's light is its light link's, which selects the CPU's light over the same stage within 2 × 10⁻⁵ (`gpu_presence_atmospheric_light_matches_the_cpu`, [the per-frame light](../design/gpu-preview.md#spatial-programs)). On the byte path the boundary holds each 8-bit code's value as a half float where the CPU reads it in `f32`, and Dehaze's recovery and Texture's gain amplify that rounding; the one large maximum is a near-black pixel ([below](#isolated-near-black-pixels)).

### The corpus at Fit

`gpu_presence_corpus_at_fit`, the shared harness the colour programs were qualified with ([GPU colour programs at Fit](#gpu-colour-programs-at-fit)), since deleted: the desktop's Fit job for the CPU frame, the CPU proxy of that build, the same stack planned from the Presence layer over the same proxy source for the GPU frame, the RAW cells planned for the linear path over an `rgba32float` boundary. The light is taken on the GPU (the harness holds no store of the worker's), which over a whole Fit stage selects the CPU's blocks. Each pair judged by `cargo xtask preview-error --class spatial`: all 49 measured pairs pass, the zone plate's seven reported in [the qualification](#the-corpus-error-report). Bounds 1716 × 1508; the `test` profile build at `24c711ab`.

| Recipe | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Max | Charged |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Texture, 24 MP / 60 MP JPEG | 0.0001 / 0.0001 | 0.01 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 0.29 / 0.35 | 87.5 / 80.4 MB |
| Texture, Presence fixture (exact) | 0.0000 | 0.01 | 0.00 | +0.000 | 0.40 | 66.5 MB |
| Texture, Z6 / X100VI / Air 2S | 0.026 / 0.0001 / 0.024 | 0.10 / 0.00 / 0.12 | 0.91 / 0.00 / 0.72 | +0.000 / +0.000 / +0.000 | 1.64 / 1.14 / 1.65 | 107.6 / 103.2 / 134.6 MB |
| Clarity, 24 MP / 60 MP JPEG | 0.0011 / 0.0014 | 0.05 / 0.04 | 0.00 / 0.00 | +0.000 / +0.000 | 0.35 / 0.55 | 35.4 / 33.6 MB |
| Clarity, Presence fixture (exact) | 0.0085 | 0.04 | 0.33 | +0.008 | 0.40 | 29.9 MB |
| Clarity, Z6 / X100VI / Air 2S | 0.032 / 0.0000 / 0.034 | 0.16 / 0.00 / 0.16 | 0.96 / 0.00 / 0.83 | −0.000 / −0.000 / −0.000 | 1.79 / 1.09 / 1.60 | 67.5 / 51.1 / 82.6 MB |
| Dehaze, 24 MP / 60 MP JPEG | 0.0000 / 0.0002 | 0.01 / 0.02 | 0.00 / 0.00 | +0.000 / +0.000 | 0.46 / 0.34 | 40.5 / 38.1 MB |
| Dehaze, Presence fixture (exact) | 0.011 | 0.19 | 0.32 | −0.001 | 0.69 | 33.5 MB |
| Dehaze, Z6 / X100VI / Air 2S | 0.026 / 0.0006 / 0.030 | 0.13 / 0.01 / 0.19 | 0.73 / 0.00 / 0.73 | −0.000 / +0.000 / −0.000 | 1.80 / 1.18 / 2.8 | 71.4 / 56.2 / 87.7 MB |
| Texture and Clarity, 24 MP / 60 MP JPEG | 0.0010 / 0.0009 | 0.03 / 0.04 | 0.00 / 0.00 | +0.000 / +0.000 | 0.55 / 0.55 | 90.4 / 83.1 MB |
| Texture and Clarity, Presence fixture (exact) | 0.0035 | 0.03 | 0.25 | +0.002 | 0.40 | 68.6 MB |
| Texture and Clarity, Z6 / X100VI / Air 2S | 0.040 / 0.0001 / 0.038 | 0.16 / 0.01 / 0.15 | 0.98 / 0.00 / 0.86 | +0.000 / +0.000 / +0.000 | 1.76 / 1.05 / 1.64 | 109.9 / 106.1 / 137.5 MB |
| All three, 24 MP / 60 MP JPEG | 0.0013 / 0.0009 | 0.04 / 0.04 | 0.00 / 0.00 | +0.000 / +0.000 | 0.54 / 0.36 | 96.4 / 88.5 MB |
| All three, Presence fixture (exact) | 0.035 | 0.49 | 0.64 | +0.002 | 1.31 | 72.9 MB |
| All three, Z6 / X100VI / Air 2S | 0.042 / 0.0010 / 0.042 | 0.17 / 0.02 / 0.22 | 0.91 / 0.00 / 0.80 | +0.000 / +0.000 / +0.000 | 6.00 / 1.22 / 14.6 | 114.5 / 112.1 / 143.6 MB |
| All three at −100, 24 MP / 60 MP JPEG | 0.0011 / 0.0015 | 0.05 / 0.08 | 0.00 / 0.00 | +0.000 / +0.000 | 0.77 / 0.81 | 96.4 / 88.5 MB |
| All three at −100, Presence fixture (exact) | 0.0009 | 0.02 | 0.00 | +0.001 | 0.26 | 72.9 MB |
| All three at −100, Z6 / X100VI / Air 2S | 0.012 / 0.0001 / 0.016 | 0.08 / 0.00 / 0.18 | 0.52 / 0.00 / 0.56 | +0.000 / +0.000 / −0.000 | 1.41 / 0.74 / 1.36 | 114.5 / 112.1 / 143.6 MB |
| All three masked by a radial, 24 MP / 60 MP JPEG | 0.0007 / 0.0006 | 0.07 / 0.05 | 0.00 / 0.00 | +0.000 / +0.000 | 1.02 / 0.55 | 96.4 / 88.5 MB |
| All three masked, Presence fixture (exact) | 0.0038 | 0.48 | 0.00 | −0.000 | 1.31 | 72.9 MB |
| All three masked, Z6 / X100VI / Air 2S | 0.024 / 0.0000 / 0.022 | 0.12 / 0.01 / 0.12 | 0.75 / 0.00 / 0.69 | +0.000 / +0.000 / +0.000 | 1.67 / 0.99 / 14.6 | 114.5 / 112.1 / 143.6 MB |

The Z6 and Air 2S cells carry the lens profile their first open commits, which the geometry tail draws after the spatial step, and their means are mostly the tail's, as for the colour programs. Its `rgba32float` intermediate holds their signed ΔL\* within ±0.0001, where an `rgba16float` one written by the M4's own conversion toward zero held them at −0.004 to −0.010; the X100VI commits none, and its `f32` boundary leaves the GPU frame within a fraction of a code of the CPU's. The masked recipe's radial selects part of each frame: its masked CPU frame equals the unmasked one on 5 to 10% of the pixels, where coverage is whole. [the qualification](#the-corpus-error-report) measures the zone plate. The largest maxima are the Air 2S's near-black pixels ([below](#isolated-near-black-pixels)).

**Memory.** "Charged" is what the photo surface's slot drawing the plan charges the GPU-preview budget, the figure its `gpu_preview_in_use_bytes` reports (a surface test holds the two equal): the boundary, the output in the photograph's size bucket and its uniform, the words and blocks buffers, and the planes. With all three fields on the 60 MP JPEG at Fit (a 1716 × 1030 proxy), the planes take 50.5 MB: about 28.6 bytes a pixel, Texture's full-resolution planes 24 of them, of which its apply keeps only its one-channel band's 4 ([design](../design/gpu-preview.md#plane-sharing-and-precision)). The table's charges were measured while Texture's band took a two-channel plane, 4 bytes a texel of the boundary more than it takes now, in every row that holds Texture: there this slot held 88.5 MB (84.4 MiB) of the 2 GiB budget, its planes 57.6 MB. One slot is held at a time, so this is its peak; a replacement overlaps the slot it replaces until that retires. The 100% figures are in [GPU previews at 100%](#gpu-previews-at-100).

### Isolated near-black pixels

Over a half-float boundary, the largest maxima, 108.8 on the Air 2S and 11.2 on the Z6 with all three fields, were single pixels: 45 of the Air 2S's 1.96 M pixels differ by more than eight codes, 14 of the Z6's 1.51 M. They need not be dark in the boundary (the Air 2S's reach 0.2 to 0.3 there): Dehaze at +100 recovers them to channels of mixed sign whose luminance is within about 10⁻⁴ of zero, and Texture's and Clarity's luminance-ratio reconstruction divides by that luminance. The half-float boundary's rounding, amplified by Dehaze's recovery, moves the luminance by more than its size, often across zero, so the ratio changes entirely: the Air 2S's worst pixel leaves Dehaze with luminance 1.2 × 10⁻⁴ over the half-float boundary and −3.1 × 10⁻⁶ over the same pixels unrounded, and draws green where the CPU draws saturated magenta (`gpu_presence_near_black_pixels`). No statistic the limits judge notices them.

`gpu_presence_near_black_variants` measured two remedies over every Presence recipe on the three RAWs (21 cells, lens reset) at Fit and on two full-resolution crops the size of the 1796 × 1660 100% view, the centre and the region round the Fit frame's worst pixel, each rendered by the CPU as a photograph of its own (so Dehaze's light is the crop's on both sides). The CPU frame is unchanged throughout.

| Boundary and near-black rule | Outliers, Fit / 100% centre / 100% worst | Largest ΔE00, Fit / 100% |
| --- | ---: | ---: |
| `rgba16float`, the CPU's rule (additive below 10⁻⁶) | 63 / 67 / 89 | 108.8 / 25.2 |
| `rgba32float`, the CPU's rule | 3 / 0 / 2 | 17.3 / 8.1 |
| `rgba16float`, additive below 10⁻⁵ | 150 / 183 / 184 | 108.8 / 25.2 |
| `rgba16float`, additive below 10⁻⁴ | 1,458 / 2,181 / 1,913 | 96.5 / 66.9 |
| `rgba16float`, additive below 10⁻³ | 18,693 / 40,453 / 27,760 | 96.5 / 83.0 |
| `rgba16float`, additive below 2⁻⁸ of \|r\| + \|g\| + \|b\| | 686 / 781 / 834 | 96.5 / 82.5 |
| `rgba32float`, additive below 2⁻⁸ of \|r\| + \|g\| + \|b\| | 665 / 796 / 827 | 96.5 / 82.5 |

The four statistics barely move: over the 21 cells an `rgba32float` boundary leaves the worst mean, p99 and signed ΔL\* within 0.01 of the half-float boundary's at both scales, and lowers the Air 2S's worst block with all three fields at Fit from 0.50 to 0.25. Of its remaining pixels, the Z6's leaves Dehaze with luminance 1.2 × 10⁻⁶, beside the CPU's own 10⁻⁶ threshold, where the filters' `f32` arithmetic decides which side of it a frame falls. It holds eight bytes a texel more: all three fields charge 77.8 rather than 65.7 MB on the Z6 at Fit and 95.9 rather than 80.2 MB on the X100VI and Air 2S, and 137.0 rather than 113.1 MB over a 1796 × 1660 region at 100%, before any margin the region adds, of the GPU-preview budget. A wider near-black rule in the GPU apply cannot follow the CPU, which keeps the ratio down to 10⁻⁶ (the GPU already holds the CPU's rule and its constant): every rule measured adds outliers. On this evidence the boundary's format is decided by path: a plan of the linear path, a RAW's, holds its boundary as `rgba32float`, and a JPEG's stays `rgba16float`. The geometry pass also preserves the RAW linear range in an `rgba32float` intermediate; a JPEG uses `rgba8unorm` when its segment boundary quantizes. `GpuBoundary` carries its format, chosen from the plan's linear flag (`BoundaryFormat::of`) where the draft's boundary is planned, and the surface derives the boundary in it from the source it holds ([GPU previews](../design/gpu-preview.md#the-gpu-source)). Over it, with the lens profile drawn, the corpus's largest maxima with all three fields are 14.1 on the Air 2S and 6.0 on the Z6.

### Reproducing it

```sh
cargo test -p luxforge-app gpu_presence -- --nocapture
LUXFORGE_GPU_CORPUS_OUTPUT=/tmp/NEW_DIR \
LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
cargo test -p luxforge-app gpu_presence_near_black_variants -- --ignored --nocapture
LUXFORGE_GPU_CORPUS_OUTPUT=/tmp/NEW_DIR LUXFORGE_NEAR_BLACK_CELL=presence-all/raw-air2s \
LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
cargo test -p luxforge-app gpu_presence_near_black_pixels -- --ignored --nocapture
```

The near-black run writes `near-black.json`, every cell's figures for every variant at each scale; its Fit cell is now the development reduced to the size the picture at rest's plan gives and drawn as a photograph of its own on both sides, where the figures above drew the proxy, so a run today measures a different Fit frame. The corpus harness, `gpu_presence_corpus_at_fit`, is deleted with the CPU proxy it compared against. The release gate runs the Presence family at Fit against the reference renderer's frame, not the CPU proxy, so it reproduces the comparison under today's contract, not these figures, as [for the colour programs](#gpu-colour-programs-at-fit):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families presence
```

## GPU preview compile cost

What compiling one program sequence costs the photo surface's compile thread ([GPU previews](../design/gpu-preview.md#where-the-code-lives)): the stage's own `compile` from the programs' `naga` checks to the driver's pipelines, the content pass's and a spatial step's, timed on the calling thread for each sequence a Fit drag of the colour modules draws. A functional measurement for the design's choice of one pipeline per sequence compiled off the interface thread, not a timing gate.

- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The `test` profile build of `luxforge-app` at `342d2842`, `gpu_preview_compile_cost_per_sequence`, three runs in new processes.
- **Figures.** Basic (four units) 2.8 to 2.9 ms; the Tone curve 1.4 ms; the Mixer 2.0 ms; the Vignette 1.4 ms; Basic under a curve and a mixer (six steps) 3.8 to 3.9 ms; Basic under every colour module (seven) 4.2 to 4.3 ms. A second compile of the same sequence in the same process is within 0.4 ms of the first.
- **The driver's cache.** Metal keeps compiled shaders on disk across processes, so these are sequences it had seen. The first run after the output encoding changed every pass's text took 97.4 ms for the six-step sequence, the one cold figure observed; the others were already cached. A cold compile is what the compile thread and the warm list keep off the interface thread.

```sh
cargo test -p luxforge-app gpu_preview_compile_cost_per_sequence -- --ignored --nocapture
```

### Presence and Detail sequences in the editor

What each program sequence the `gpu-preview` scenario compiles costs the editor's compile thread, from the queue to a kept pipeline, on the release editor of main before the GPU preview follow-ups (`40c3cca5`) and of the follow-ups with half-precision planes (`0462e063`), each with one line printed per compile and nothing else changed. Runs alternate the two builds, each on a cold Metal shader cache (a background bundle identifier of its own, whose cache starts empty) and then warm (the same identifier again), three rounds. Apple M4 Pro, one-minute load 2.6 to 15.2 while they ran. Medians, milliseconds; "new" is the spatial pass pipelines the sequence created, the others found in the passes' cache.

| Sequence | Main, cold | Follow-ups, cold | Main, warm | Follow-ups, warm |
| --- | ---: | ---: | ---: | ---: |
| A Presence drag: Presence in its GPU shape, 22 passes | 1,119 (13 new) | 1,161 (14 new) | 25 | 30 |
| Basic under Presence, Dehaze alone (9 passes) | 1,075 | 1,096 | 36 | 42 |
| Basic under Presence, Dehaze and Clarity (14) | 217 | 231 | 9 | 20 |
| Basic under Presence, all three (22) | 734 | 758 | 22 | 25 |
| The Tone curve under Presence, 9 / 14 / 22 passes | 767 / 158 / 512 | 793 / 172 / 528 | 22 / 9 / 13 | 22 / 20 / 19 |
| The Mixer under Presence, 9 / 14 / 22 | 820 / 172 / 550 | 817 / 197 / 641 | 25 / 12 / 18 | 39 / 29 / 29 |
| The masked Basic under Presence, 9 / 14 / 22 | 990 / 217 / 731 | 996 / 236 / 760 | 34 / 12 / 20 | 56 / 24 / 22 |
| A Detail drag, the colour layers and Presence after it, 9 / 14 / 22 | 1,128 / 231 / 789 | 1,150 / 254 / 997 | 37 / 14 / 31 | 48 / 106 / 51 |
| A Detail drag before Presence was committed | 463 | 482 | 11 | 12 |
| A colour sequence (Basic, the curve, the mixer, the vignette) | 45 to 79 | 46 to 73 | 1 to 4 | 1 to 4 |
| Everything a run compiles | 11,738 | 12,657 | 410 | 675 |

- **Compile time barely moved.** A Presence drag's own sequence costs 4% more cold, and every sequence before Presence 0 to 4%, but for two that now create more pass pipelines: the Mixer before all three Presence fields (+17%) and a Detail drag with all three after it (+26%, 14 new against 8), whose Presence passes store into Detail's free half-precision planes, so they share fewer modules with the other sequences. A run's whole compile work is 8% more cold and 265 ms more warm.
- **What the scenario waited for.** On these builds a drag of a Presence layer, and a drag of every colour layer under it, each compiled its own Presence passes, since every pass's module carried the programs of the steps before it: about a second a sequence on a cold cache; since, each spatial step is a link of its own whose passes hold its program alone ([design](../design/gpu-preview.md#where-the-code-lives)). The Dehaze commit warms six, the Presence drag's own last, and on both builds that one was ready 5.9 to 6.4 s after the commit (main 5.87 to 5.95 s, the follow-ups 5.94 to 6.39 s, 7.3 s in one run on a host at load 30 to 60). The scenario's fixed 4 s quiet after the Clarity commit, and the 1.5 s while the drag's boundary arrived, covered that only on an idle host, so a cold cache under load failed the Texture drag's first GPU tick (`compiling`) on either build. The scenario now waits for the warm list to compile ([design](../design/gpu-preview.md#where-the-code-lives)).
- **Warm**, every sequence compiles in 1 to 106 ms, the Presence drag's in 25 to 30.
- **With the wait.** On the follow-ups with the scenario's `gpu_warmed` steps, five runs on a cold cache passed: the wait after the Presence commits took 6.3 to 6.6 s, and 8.5 to 8.6 s with twelve busy loops beside the editor, each longer than the fixed quiet that preceded it; the waits after the Texture and Clarity drags' releases took 3.1 to 4.5 s and 1.5 to 1.7 s. Eight consecutive runs on the shared warm cache, the first two on a freshly built editor, passed at loads 3.4 to 9.7.
- **At 100%** (`gpu-preview-zoom`) a percentage view's plans are not warmed, and each drag compiles its own when its boundary is first drawn: cold, a Presence drag's region sequence in 1.09 to 1.12 s, a Basic drag under Presence's in 0.57 s and a Detail drag with Presence after it (Dehaze behind Detail) in 1.85 to 1.90 s, warm in 23 to 52 ms. Each of those drags is now held after its first tick until that compile has ended, at least 4 s and at most 60 s: two cold runs, one beside twelve busy loops, and one warm passed, every held step ending at 4.0 to 4.2 s.



### Warming at launch and open

The warm-up's own duration as the editor records it (`gpu_warm_up`, [GPU previews](../design/gpu-preview.md#warming-at-launch-and-open)): from a warm list handed to the idle compile thread until its queue drained, and when the newest list's open stack's part, with the frame's own sequences, had compiled. Release editor of `claude/gpu-first-task-009`, the `gpu-preview` scenario on the M4 Pro (Metal), 5 October 2026: one run per row, functional figures, not a timing gate.

| Run | Shader cache | First warm-up | Open stack's part | Reference frame on screen before the GPU's |
| --- | --- | ---: | ---: | ---: |
| `LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=task009-cold-1`, the first launch of the new build | A bundle identifier of its own, its Metal cache empty | 4,389 ms, 11 sequences over two lists (the release's list joined it) | 1,510 ms (the second list's) | not recorded: the idle check, then still unaware of warm-ups, failed the run |
| `task009-cold-2`, the next launch | Its own, empty | 1,525 ms, 6 sequences | 41 ms | 38 ms (`surface_frame_drawn` `cpu` at 957 ms, `gpu` at 995 ms) |
| The shared background bundle | Warm | 37 ms, 6 sequences | 2 ms | |

The second cold run is faster than the first although its bundle's cache was empty too: macOS also keeps a compiler cache outside the bundle's, which the first run filled and which was not cleared, so neither row is a whole-system cold cache. Later warm-ups in both cold runs, as each commit warmed its stack, took 0.3 to 0.7 s and compiled only what the stack added.

```sh
cargo build --release --locked -p luxforge-app
LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=NEW_SUFFIX cargo run --release --locked --package xtask -- smoke --scenario gpu-preview --output NEW_DIR
```

### Compacted pass modules

Every spatial pass's module reaches wgpu as the `naga` module the surface validated, compacted to its entry point ([GPU previews](../design/gpu-preview.md#where-the-code-lives)): `naga` leaves a module of one entry point uncompacted, so the driver compiled the whole spatial program, every kernel and apply, for every pass. `gpu_preview_spatial_compile_cost_on_a_cold_cache` compiles, in one process and in the order the editor's warm lists do, the links of seven Presence and Detail sequences — a Presence drag's own, drags of colour layers under it and a Detail drag with Presence after it — which create 20 pass pipelines in all, the links sharing the rest. Every kernel and apply opens with a test of its words against the process's own constant, so the driver's shader cache holds none of them; a second run handed the first run's constant measures them warm. The headless `test` profile build of the chain of links, one binary compacting and one handing wgpu the WGSL text, alternated three times each on the `Apple M4 Pro` adapter (Metal), 3 October 2026, at a one-minute load of 2.5 to 2.7. Cold, milliseconds:

| Sequence | Compacted | WGSL text |
| --- | ---: | ---: |
| A Presence drag: Presence in its GPU shape, 22 passes from 14 pipelines | 910 / 941 / 951 | 1,197 / 1,180 / 1,174 |
| A Detail drag, Presence after it | 390 / 384 / 371 | 417 / 446 / 436 |
| All seven | 1,499 / 1,401 / 1,394 | 1,689 / 1,702 / 1,685 |

Compacting takes about a fifth off a Presence link's cold compile and 12 to 18% off the seven. It removes only what the entry point never reaches: before the chain, frames hashed the same from the text, the uncompacted module and the compacted one over ten plans, and every GPU test passes compacted.

```sh
cargo test -p luxforge-app gpu_preview_spatial_compile_cost_on_a_cold_cache -- --ignored --nocapture
LUXFORGE_COMPILE_CONSTANT=EARLIER_RUNS_CONSTANT cargo test -p luxforge-app gpu_preview_spatial_compile_cost_on_a_cold_cache -- --ignored --nocapture
```

## GPU Detail program

The Detail program ([GPU previews](../design/gpu-preview.md#spatial-programs)) on the M4, against the CPU per kernel, per unit and on the [corpus](../../fixtures/preview/corpus.json)'s Detail recipes at Fit. These are the figures the program was enabled on: on that build a Detail stack settled from the exact render, and its GPU frame was judged against the CPU's moving proxy it stood in for (owner, 2026-10-02, [decisions](../decisions.md#gpu-previews)). Detail's exact-derived display and the CPU proxy it was judged against are deleted, and the GPU now draws Detail's picture at rest too. Pixel and arithmetic measurements, not timings.

### Scope

- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal (`wgpu-hal` 27.0.4, Metal's default fast math). The `test` profile build of `luxforge-app` at `bbfbe213` per kernel and per unit, with half-precision planes written rounded to the nearest half ([plane precision](#plane-precision)), and at `61592997` on the corpus. One run each; the pixels are deterministic.
- **The readback.** Every GPU figure is the photo surface's own spatial step, its generated pass modules and frame module, run headlessly by the surface's qualification readback.
- **The CPU.** Per kernel, the production code of `modules/detail/{filters,denoise,sharpen}.rs` over a whole frame (`luxforge_core::qualification::detail`, test builds only). Per unit, the CPU's frame of the same stack: the exact render at full resolution, and the units compiled at a proxy's scale over the whole frame.

### Per kernel

Each kernel through the surface's passes over synthetic planes (Oklab-like lightness and chroma with a ramp, a step edge, a grating and noise, held as half floats as the boundary holds them) against the CPU kernel it transcribes. The largest absolute difference:

| Kernel | Cases | Largest difference |
| --- | --- | ---: |
| Oklab of the unit's input | the extreme grid: in range, past white, below black and a half float's extremes | 1.1 × 10⁻⁵ of the value or of one, whichever is larger, over colours in [−1, 16]; finite everywhere |
| Separable smoothing, every channel | the B3 kernel at spacings 1, 2, 4 and 8; Gaussians of σ 0.05 to 7.9 (2 to 48 taps), alike and different on the two axes | 0 (exact) for B3; 2.4 × 10⁻⁷ |
| Sharpening's blur and guide together | Radius 0.5, 1 and 3 at full resolution, 1 at a scale of 0.29 and 3 at 0.1716 × 0.1717 | 1.2 × 10⁻⁷ |
| One level's band, energy and shrinkage | every level's thresholds at Luminance and Colour 40 and 40, 100 and 100, 60 alone and 70 alone; details 0, 0.5 and 1 with 0.2; the first level and later ones (45 cases) | 3.2 × 10⁻⁸ |
| Sharpening's change of lightness | Amount, Detail and Masking at 50, 25, 0; 150, 100, 0; 150, 0, 100; 60, 25, 50; 1, 50, 30 | 1.2 × 10⁻⁷ |
| Reconstruction, both applies | a photograph's range; the extreme grid | 1.8 × 10⁻⁶ relative; 3.9 × 10⁻⁵ over its colours in [−1, 16]. A zero change returns the input exactly, a snapped grey stays grey, and every output is finite |

The Oklab and reconstruction maxima are where an extended colour's LMS component cancels to near zero, where the cube root's slope magnifies the one rounding the backend's fused multiply-adds change. `tanh`, which the limiter takes, is within 1.9 × 10⁻⁷ absolute over [0, 16] (501 ulp, 3.0 × 10⁻⁵ relative) and 5.1 × 10⁻⁸ below 1/32, where its relative error reaches 3.1 × 10⁻² ([precision](../design/gpu-preview.md#qualifying-a-program)).

### Per unit

Noise reduction, sharpening and both, at the study's moderate, noise-stress and sharpen-stress settings, Luminance 60 alone, Colour 70 alone and a narrow masked sharpening, over a synthetic photograph (flat patches at four levels under signal-dependent noise, a slanted edge, a grating over chroma blotches and a neutral ramp) at 480 × 320 and 1536 × 1024 on the linear path: at full resolution, unmasked and masked by a feathered radial, and at a Fit proxy's scale (0.29 and 0.256). 36 cases. Worst of each statistic:

| Measured | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Program output through the reference quantizer | 0.035 | 0.330 | 0.90 | −0.0003 | 1.83 |
| Codes the surface draws, its encoding the CPU quantizer's | 0.035 | 0.330 | 0.90 | −0.0003 | 1.83 |
| Byte path, 960 × 640, the three study settings | 0.036 | 0.321 | 0.89 | −0.0003 | 2.04 |

The figures are the half-precision planes' ([plane precision](#plane-precision)): with every plane at full precision the program's output is within a mean of 0.0001, a worst block of 0.006 and a p99 of 0.00. At a proxy's scale the output is within 1.4 × 10⁻³ of the CPU's in linear light. No output is non-finite. Both units run their 17 passes from 6 pipelines, and drags to other settings of the same units compile none.

### Plane precision

The former RAW geometry-tail candidate used an `rgba16float` intermediate and lost extended linear range. The current tail uses `rgba32float`; a synthetic native GPU identity-tail regression verifies bit-for-bit `f32` preservation, and the native RAW geometry test matches CPU output codes exactly. Over the corpus on the three RAWs, the colour, mask, Presence and Detail harnesses at Fit and every family at 100%, the `rgba32float` tail draws every RAW cell within the limits, its geometry cells within a signed ΔL\* of ±0.0001, and its intermediate charges each such slot its `f32` bytes ([GPU previews at 100%](#gpu-previews-at-100)); an in-editor memory measurement of a photo-sized RAW drag remains outstanding.

Which spatial planes are held at half precision ([design](../design/gpu-preview.md#plane-sharing-and-precision)), plane by plane: `gpu_detail_plane_precision_is_measured` draws each stack in the GPU shape a drag draws over the synthetic photograph at 1536 × 1024 on the linear path, with every plane at full precision, as shipped, and with each full-resolution plane alone at half precision (`rgba16float`), against the CPU's exact frame; the largest linear difference is against the full-precision program. One run on the M4, at `bbfbe213`.

| Stack, planes at half precision | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Largest linear Δ |
| --- | ---: | ---: | ---: | ---: | ---: |
| Detail moderate, none | 0.0001 | 0.006 | 0.00 | +0.0000 | – |
| Detail moderate, the current and next levels' smoothing (each alone) | 0.024 / 0.025 | 0.28 / 0.36 | 0.87 / 0.87 | +0.0000 / +0.0002 | 6.1 / 5.0 × 10⁻⁴ |
| Detail moderate, the horizontal pass and change the levels take in turn (each alone) | 0.013 / 0.005 | 0.21 / 0.11 | 0.58 / 0.00 | +0.0000 / +0.0000 | 3.7 / 3.1 × 10⁻⁴ |
| Detail moderate, all four, as shipped | 0.035 | 0.33 | 0.89 | +0.0000 | 1.0 × 10⁻³ |
| Detail noise stress / sharpen stress, all four | 0.032 / 0.027 | 0.33 / 0.08 | 0.89 / 0.83 | −0.0000 / −0.0000 | 1.0 / 0.9 × 10⁻³ |
| Detail then Texture and Clarity, Texture's coefficients, band and fine smoother in Detail's planes | 0.067 | 0.42 | 1.00 | −0.0003 | 2.0 × 10⁻³ |
| Detail then all three fields, the same | 0.087 | 0.43 | 1.05 | +0.0010 | 2.8 × 10⁻³ |
| Texture and Clarity alone, none (as shipped) | 0.0003 | 0.007 | 0.00 | +0.0000 | – |
| Texture and Clarity alone, the running sums | 0.19 | 0.39 | 0.97 | +0.0001 | 1.6 × 10⁻² |
| Texture and Clarity alone, the coefficients and band | 0.071 | 0.19 | 0.91 | +0.0000 | 2.3 × 10⁻³ |
| Texture and Clarity alone, the fine smoother | 0.087 | 0.24 | 1.01 | −0.0000 | 1.3 × 10⁻³ |

- **Shipped.** Detail's planes at half precision, holding sharpening's scratch, each unit's apply reading a plane of its own; Texture's planes, Clarity's and Dehaze's on their 4× grids and every accumulator at `f32`, Presence running in a link of its own after Detail's, so Texture's planes take none of Detail's ([design](../design/gpu-preview.md#plane-sharing-and-precision)). Every case is far inside the spatial limits and none is biased.
- **The offsets.** Detail holds lightness less one half, and Texture its fine smoother less one half, where a half's step is finer. Without them all four Detail planes at half precision gave a mean of 0.078 (worst block 0.53, p99 0.97) on the moderate stack and the chain with all three fields 0.152.
- **Rounding.** Before each write to a half-precision spatial plane the surface rounds to the nearest half, ties to even. Without it the M4 converts a storage write toward zero: of 49,152 values in every binade a frame holds, of both signs, 24,614 were held toward zero and the rest at the nearest, which every one is with the rounding (`a_half_planes_texels_are_the_nearest_half`). Through noise reduction's four chained smoothings that drew the moderate stack darker by a signed ΔL\* of −0.12 to −0.13 (mean 0.26). The former RAW-tail `rgba16float` candidate also narrowed its intermediate and drew RAW geometry darker; the current `rgba32float` path preserves the linear values without half rounding. The older corpus report below records the failed candidate and does not qualify current RAW geometry accuracy or memory use.

### The corpus at Fit

`gpu_detail_corpus_at_fit`, the shared harness, since deleted, over the corpus's six single-layer Detail recipes (its three chained ones are [below](#after-detail-presence)): the study's moderate, noise-stress and sharpen-stress settings, each unmasked and through the radial mask, on every source this host has. The GPU frame was planned from the Detail layer over the same proxy source the worker rendered from, with the units compiled at the proxy's scale as the CPU's proxy compiled them, and drawn with the surface's own encoding, which is the CPU quantizer's; the RAW cells were planned for the linear path over an `f32` boundary, and the Z6 and Air 2S keep the lens profile their first open commits, which the geometry tail warps. Each cell was judged against the CPU's moving proxy, and the preview worker's exact phase reduced to the Fit bounds, the frame the stack settled to, was written beside the pair: the CPU proxy against it is the jump settlement made on the CPU path. Each pair judged by `cargo xtask preview-error --class spatial`: all 36 exit 0. Bounds 1716 × 1508.

Mean, worst 16 × 16 block, p99 and signed mean ΔL\* of the drawn frame against the CPU's moving proxy, then the mean, worst block and p99 of that proxy against the exact-derived frame it settled to, which no limit judged:

| Recipe, source | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Settle jump: mean | Worst block | p99 | Charged |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Moderate, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.02 / 0.01 | 0.00 / 0.00 | +0.000 / −0.000 | 0.00 / 0.00 | 1.18 / 0.58 | 0.07 / 0.00 | 126.7 / 115.8 MB |
| Moderate, Presence fixture (exact) | 0.006 | 0.02 | 0.36 | +0.000 | – | – | – | 94.2 MB |
| Moderate, Z6 / X100VI / Air 2S | 0.023 / 0.011 / 0.025 | 0.09 / 0.08 / 0.12 | 0.73 / 0.52 / 0.74 | −0.000 / +0.000 / −0.000 | 0.21 / 0.07 / 0.70 | 1.50 / 0.44 / 4.90 | 1.54 / 0.90 / 5.22 | 137.9 / 142.4 / 173.8 MB |
| Noise stress, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.03 / 0.01 | 0.00 / 0.00 | −0.000 / −0.000 | 0.02 / 0.01 | 4.40 / 2.32 | 0.26 / 0.11 | 111.0 / 101.6 MB |
| Noise stress, Presence fixture (exact) | 0.001 | 0.02 | 0.00 | +0.000 | – | – | – | 83.1 MB |
| Noise stress, Z6 / X100VI / Air 2S | 0.022 / 0.011 / 0.022 | 0.09 / 0.07 / 0.12 | 0.71 / 0.52 / 0.70 | +0.000 / +0.000 / −0.000 | 0.17 / 0.06 / 0.44 | 1.00 / 0.56 / 2.62 | 1.15 / 0.90 / 2.78 | 125.7 / 126.7 / 158.1 MB |
| Sharpen stress, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.00 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 0.00 / 0.00 | 1.32 / 0.65 | 0.00 / 0.00 | 87.5 / 80.4 MB |
| Sharpen stress, Presence fixture (exact) | 0.000 | 0.00 | 0.00 | +0.000 | – | – | – | 66.5 MB |
| Sharpen stress, Z6 / X100VI / Air 2S | 0.023 / 0.000 / 0.027 | 0.10 / 0.01 / 0.14 | 0.74 / 0.00 / 0.76 | +0.000 / +0.000 / +0.000 | 0.47 / 0.20 / 1.37 | 2.25 / 1.08 / 6.36 | 2.43 / 0.97 / 7.49 | 107.6 / 103.2 / 134.6 MB |
| Moderate, masked, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.02 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 0.00 / 0.00 | 0.23 / 0.15 | 0.00 / 0.00 | 126.7 / 115.8 MB |
| Moderate, masked, Presence fixture (exact) | 0.000 | 0.02 | 0.00 | +0.000 | – | – | – | 94.2 MB |
| Moderate, masked, Z6 / X100VI / Air 2S | 0.022 / 0.001 / 0.021 | 0.10 / 0.05 / 0.12 | 0.71 / 0.00 / 0.69 | −0.000 / +0.000 / +0.000 | 0.37 / 0.09 / 1.08 | 1.59 / 0.38 / 5.22 | 1.82 / 0.93 / 5.55 | 137.9 / 142.4 / 173.8 MB |
| Noise stress, masked, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.03 / 0.01 | 0.00 / 0.00 | −0.000 / +0.000 | 0.00 / 0.00 | 1.08 / 0.81 | 0.00 / 0.00 | 111.0 / 101.6 MB |
| Noise stress, masked, Presence fixture (exact) | 0.000 | 0.01 | 0.00 | +0.000 | – | – | – | 83.1 MB |
| Noise stress, masked, Z6 / X100VI / Air 2S | 0.021 / 0.002 / 0.021 | 0.09 / 0.05 / 0.12 | 0.70 / 0.00 / 0.68 | −0.000 / +0.000 / +0.000 | 0.35 / 0.08 / 1.06 | 1.58 / 0.31 / 5.22 | 1.65 / 0.92 / 5.51 | 125.7 / 126.7 / 158.1 MB |
| Sharpen stress, masked, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.01 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 0.00 / 0.00 | 0.33 / 0.24 | 0.00 / 0.00 | 87.5 / 80.4 MB |
| Sharpen stress, masked, Presence fixture (exact) | 0.000 | 0.01 | 0.00 | +0.000 | – | – | – | 66.5 MB |
| Sharpen stress, masked, Z6 / X100VI / Air 2S | 0.022 / 0.000 / 0.021 | 0.09 / 0.00 / 0.12 | 0.71 / 0.00 / 0.69 | +0.000 / +0.000 / +0.000 | 0.43 / 0.09 / 1.13 | 2.18 / 0.48 / 5.22 | 2.34 / 0.93 / 5.62 | 107.6 / 103.2 / 134.6 MB |

Every cell is within the spatial limits: at worst a mean of 0.027, a worst block of 0.14 and a p99 of 0.76, on the Z6 and Air 2S, where the geometry tail's coordinate grid places the lens warp's samples, and no signed ΔL\* past ±0.001; on the X100VI the half-precision planes leave a mean of 0.011 and a worst block of 0.08, and on the JPEGs the GPU frame is within a worst block of 0.03 of the proxy. The Presence fixture fits the bounds at its own size, so its Fit frame is the exact render itself, with no proxy and no settle jump, and its GPU frame runs the full-resolution B3 levels. [the qualification](#the-corpus-error-report) measures the zone plate.

**The settle jump was the CPU path's.** The moving proxy ran Detail's filters, which the design defines at full resolution, over averaged proxy pixels, so it moved when the exact-derived frame replaced it: by up to a mean of 1.37, a worst block of 6.36 and a p99 of 7.49 (sharpen stress on the Air 2S, whose sunlit glints on the sea the proxy's sharpening gives more contrast than the full-resolution sharpening keeps once reduced), and on the 24 MP JPEG by a worst block of 4.40 with noise stress (a row of small dark marks on the edge between two quadrants, darker and firmer once reduced from full resolution). The GPU frame, against the same settled frame, moves by the proxy's figures within 0.01: it leaves that jump as it is.

**Memory.** "Charged" is what the photo surface's slot drawing the plan charges the GPU-preview budget, as for Presence. Both units hold 48 bytes a pixel of half-precision planes, noise reduction 40 alone and sharpening 28 alone, each unit's apply reading a plane of its own that one pass writes ([design](../design/gpu-preview.md#plane-sharing-and-precision)). With the moderate settings on the 60 MP JPEG at Fit (a 1716 × 1030 proxy), the slot holds 115.8 MB (110.4 MiB) of the 2 GiB budget, of which the planes are 84.8 MB; over the evidence window's whole 1716 × 1508 bounds it would hold 161.7 MB (154.2 MiB). The RAWs charge more, their boundary held as `f32` and the Z6's and Air 2S's lens grid beside it: 173.8 MB at most, the Air 2S with moderate settings. One slot is held at a time, so this is its peak. A JPEG stage of 7000 × 4667 charges 2,035.0 MB (1,940.7 MiB) and fits; one past about 33 MP passes the budget (7200 × 4800 would charge 2,070.7 MiB) and takes the CPU path, naming it (`gpu_detail_planes_are_charged_to_the_budget`). At 100% a Detail slot charges 337.1 to 500.8 MB, and Detail then all three Presence fields 1,099.1 to 1,159.2 MB ([GPU previews at 100%](#gpu-previews-at-100), [after Detail, Presence](#after-detail-presence)).

### After Detail, Presence

A plan chains every spatial and restoration layer after its boundary in recipe order ([GPU previews](../design/gpu-preview.md#where-the-code-lives)), so a Detail drag with Presence in the stack, or a drag of a layer before both, is drawn on the GPU. The corpus's three chained recipes, in the Detail family, ran through `gpu_detail_corpus_at_fit` (`LUXFORGE_GPU_CORPUS_RECIPES=detail-presence,detail-presence-local,detail-presence-masked`): Detail at the moderate settings, then Presence with all three fields at +50 / +50 / +30, with Texture and Clarity alone, and with all three through the radial mask. The plan starts at the Detail layer and holds Detail's operation, then Presence's. Each cell was judged as the Detail cells were, against the CPU's moving proxy, with the settle jump beside it: the CPU proxy's mean against the exact-derived frame, then the GPU frame's. All 18 measured cells meet the spatial limits; [the qualification](#the-corpus-error-report) measures the zone plate.

| Recipe, source | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Settle jump: CPU proxy mean | GPU mean | Charged |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| All three fields, 24 MP / 60 MP JPEG | 0.001 / 0.054 | 0.04 / 0.26 | 0.00 / 0.26 | +0.000 / −0.019 | 0.36 / 0.42 | 0.36 / 0.37 | 206.4 / 187.5 MB |
| All three fields, Presence fixture (exact) | 0.004 | 0.06 | 0.25 | −0.000 | – | – | 150.3 MB |
| All three fields, Z6 / X100VI / Air 2S | 0.032 / 0.020 / 0.039 | 0.14 / 0.13 / 0.15 | 0.96 / 0.61 / 0.87 | +0.000 / +0.000 / +0.000 | 0.95 / 0.47 / 1.63 | 0.95 / 0.48 / 1.63 | 211.3 / 237.8 / 269.1 MB |
| Texture and Clarity, 24 MP / 60 MP JPEG | 0.001 / 0.001 | 0.06 / 0.05 | 0.00 / 0.00 | +0.000 / +0.000 | 0.04 / 0.04 | 0.04 / 0.04 | 200.3 / 182.1 MB |
| Texture and Clarity, Presence fixture (exact) | 0.003 | 0.05 | 0.20 | −0.000 | – | – | 146.0 MB |
| Texture and Clarity, Z6 / X100VI / Air 2S | 0.029 / 0.018 / 0.036 | 0.13 / 0.11 / 0.14 | 0.96 / 0.59 / 0.85 | −0.000 / +0.000 / −0.000 | 0.58 / 0.33 / 1.44 | 0.58 / 0.33 / 1.44 | 206.7 / 231.7 / 263.1 MB |
| All three fields, masked, 24 MP / 60 MP JPEG | 0.001 / 0.001 | 0.08 / 0.26 | 0.00 / 0.00 | +0.000 / −0.000 | 0.03 / 0.03 | 0.03 / 0.03 | 206.4 / 187.5 MB |
| All three fields, masked, Presence fixture (exact) | 0.004 | 0.04 | 0.25 | −0.002 | – | – | 150.3 MB |
| All three fields, masked, Z6 / X100VI / Air 2S | 0.020 / 0.012 / 0.024 | 0.13 / 0.08 / 0.13 | 0.70 / 0.53 / 0.73 | −0.000 / +0.000 / −0.000 | 0.39 / 0.10 / 0.78 | 0.39 / 0.10 / 0.78 | 211.3 / 237.8 / 269.1 MB |

The figures are Detail's and Presence's own, Detail's half-precision planes included ([plane precision](#plane-precision)): at worst a mean of 0.054 and a signed ΔL\* of −0.019 on the 60 MP JPEG, where Dehaze's light is the stage's, a worst block of 0.26 there too, and a p99 of 0.96 on the Z6, where the lens warp's grid places the samples (all within the spatial limits of 1.0, 2.5, 5.0 and ±0.5). Against the frame the stack settled to, the GPU frame moved by the CPU proxy's figures within 0.06. Each operation is a link of its own with its own planes, so the slots charge 146.0 to 269.1 MB at Fit.

**At 100%.** In the largest window the owner's display holds, a 3026 × 1826 region ([GPU previews at 100%](#gpu-previews-at-100)), Detail then Texture and Clarity draws on the GPU on every source, and so does Detail then all three fields. Dehaze's estimate sits behind Detail; these figures were measured with the light the whole exact frame stored, and its cells against the exact whole frame's region, the frame the view draws. A region plan now reads the light its light link computes, with Detail left out until a slot sweeps the exact prefix, which the [release gate](#gpu-qualification-against-the-reference) measures. Each slot is in the GPU's shape, every unit held, and the window is the region grown by the halos after the boundary alone, with no tile grid ([design](../design/gpu-preview.md#at-100-and-above)); Detail's link holds its half-precision planes and Presence's link its own, Texture's at full precision, with the intermediate Detail's link writes between them ([plane precision](#plane-precision)). Charged, against the 2,147.5 MB (2 GiB) budget, and the headroom it leaves:

| Recipe, source | Window | Charged | Headroom | Mean | Worst block | p99 | Signed ΔL\* |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Texture and Clarity, 24 MP JPEG | 3510 × 2310 | 780.9 MB | 1,366.5 MB | 0.002 | 0.08 | 0.04 | +0.001 |
| Texture and Clarity, 60 MP JPEG | 3778 × 2578 | 933.5 MB | 1,213.9 MB | 0.003 | 0.09 | 0.12 | +0.001 |
| Texture and Clarity, Z6 | 3418 × 2256 | 990.6 MB | 1,156.9 MB | 0.026 | 0.15 | 0.78 | +0.000 |
| Texture and Clarity, X100VI | 3626 × 2426 | 986.1 MB | 1,161.4 MB | 0.025 | 0.11 | 0.64 | +0.000 |
| Texture and Clarity, Air 2S | 3494 × 2302 | 1,032.3 MB | 1,115.2 MB | 0.049 | 0.25 | 0.90 | +0.000 |
| All three fields, 24 MP JPEG | 3644 × 2444 | 882.9 MB | 1,264.6 MB | 0.002 | 0.07 | 0.03 | +0.001 |
| All three fields, 60 MP JPEG | 3992 × 2792 | 1,099.1 MB | 1,048.4 MB | 0.004 | 0.08 | 0.22 | +0.001 |
| All three fields, Z6 | 3552 × 2390 | 1,114.3 MB | 1,033.2 MB | 0.028 | 0.15 | 0.83 | +0.000 |
| All three fields, X100VI | 3800 × 2600 | 1,134.9 MB | 1,012.5 MB | 0.025 | 0.12 | 0.64 | +0.000 |
| All three fields, Air 2S | 3628 × 2436 | 1,159.2 MB | 988.3 MB | 0.052 | 0.26 | 0.90 | +0.000 |

- **The figures** are within the spatial limits by far, none past half of any, and no signed ΔL\* past ±0.001. The masked all-three cells charge the same and draw within a worst block of 0.22 and a p99 of 0.80 (Air 2S); the zone plate's draw within a worst block of 0.02 and a p99 of 0.23, and the Presence fixture's, which fit the window whole, charge 135.3 and 139.5 MB.
- **A larger window**, such as an external display's, draws a larger region. The charge grows with the window's area, and from the measured charges the all-three chain stops fitting at a region about 36% wider and taller on the Air 2S (about 4120 × 2485), 38% on the X100VI, 39% on the Z6 and 40% on the 60 MP JPEG, and Texture and Clarity about 44% on the Air 2S (an estimate from the charges above, not a measurement). Such a region's drag took the CPU path naming `budget-exceeded` on that build; it is now drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging", `budget-reduced`), and is sharp at rest ([GPU previews](../design/gpu-preview.md#at-100-and-above)). The slot's scratch pool leaves this chain as it is, since Detail's planes with noise reduction share no texture class with Presence's; the [design](../design/gpu-preview.md#later) records the proposal that would widen the headroom, evaluating the ring only Clarity's and Dehaze's reductions read in strips. With Texture's band in one channel the corpus's heaviest slot, all three fields on the Air 2S, charges 1,123.8 MB ([the corpus](#the-corpus)).
- **A slot that replaces another** is charged only once the one it replaces has retired, when the GPU is done with it: a tick whose slot the budget holds only without the old one draws the CPU's frame, naming `budget-exceeded`, keeps its boundary and plan and asks for nothing again, and the next frame after the retirement draws on the GPU; nothing is allocated or released twice (`a_larger_plan_waits_for_the_planes_it_replaces_then_holds_its_own`, `gpu_preview_a_tick_the_budget_refuses_while_a_slot_retires_keeps_its_boundary`).

**Ticks.** Over Detail then Presence (Texture and Clarity), 27 passes, a tick runs only the passes its words change: a Clarity drag none, a Texture drag 5, a drag of the colour layer between the two Presence's 13, and a Detail drag 17, each frame equal to the bit to a run of every pass, each operation a link of its own (`gpu_presence_after_detail_reruns_only_the_passes_a_tick_changes`). Against the CPU frame of the same stacks on a synthetic photograph at two sizes, Detail's sharpening and noise reduction then three Presence settings, plain and masked, with Dehaze's light stored and taken on the GPU, the worst of 40 cases is a mean of 0.068, a worst block of 0.38 and a p99 of 0.80, its signed ΔL\* −0.001 (`gpu_presence_after_detail_meets_the_spatial_limits`): Detail's half-precision planes, and Detail's output held in the link's `rgba16float` intermediate between the two, each written rounded to the nearest half.

### A drag at Fit

A Detail drag at Fit drawn on the GPU is proven functionally, not timed:

- `cargo test -p luxforge-app gpu_detail_tests::drags` against a real owner and preview worker. In the first test, a Detail Amount drag over a photograph drawn at Fit derives its boundary, the Detail layer's input, the source reduced to the view, with its first tick and holds the frame on screen until the surface has evaluated its plan. Its later ticks are Detail's spatial step drawn on the GPU with no preview job. The release commits: the committed stack renders no proxy, its exact frame reduced to the view is the reference frame, and the GPU draws its picture at rest; the boundary stays resident for the next draft. In the second, a Basic drag after a committed Detail layer starts from that resident boundary, the stack's first layer's input, asking for none, and its ticks run Detail's spatial step, which the surface keeps by content, then Basic's colour, drawn on the GPU with no preview job.
- `cargo run --release --locked --package xtask -- smoke --scenario gpu-preview --output NEW_DIR` over the working tree on `5dccacc6`, 2026-10-03, with the Detail steps: every tick drawn on the GPU, the first included, over the resident boundary at layer 0 that the Basic drags before it left, with no boundary request and no preview job. The GPU frame equals the frame the release commits at four flat patches and two across the white cross, to the code. The slot held 16.4 MB over the 480 × 320 photograph, which fits the window at its own size, so its Fit frame is the exact render, and holds it once settled, the boundary resident for the next gesture.

### Reproducing it

```sh
cargo test -p luxforge-app gpu_detail -- --nocapture
```

The corpus harness, `gpu_detail_corpus_at_fit`, is deleted with the CPU proxy it compared against. The release gate runs the Detail family, its chained recipes among them, at Fit against the reference renderer's frame, not the CPU proxy or the exact-derived frame, so it reproduces the comparison under today's contract, not these figures, as [for the colour programs](#gpu-colour-programs-at-fit):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families detail
```

## GPU preview settle

What a person saw when a gesture settled on that build: the GPU frame on screen against the CPU frame the 150 ms dissolve ended on ([design](../design/gpu-preview.md#settle-and-the-dissolve)). A release now hands the drag's frame to the committed stack's GPU frame over the same boundary, and a dissolve into a CPU frame runs only where the reference renderer draws what the GPU could not. M4 Pro (Metal), 2026-10-02, on a shared host at load average about 25: functional figures, not timings.

**In the editor.** The `gpu-preview` scenario's Basic drag, its last frame drawn on the GPU against the settled frame its release committed, over the whole photograph: mean ΔE00 1.1 × 10⁻⁵, worst block 0.014, p99 0, signed ΔL\* −3 × 10⁻⁶, within the pointwise limits. The moved gradient's and the stroke's frames carry their handles or cursor, which the settled frames do not, so they are held to the CPU's by patch instead. Every commit that replaced a GPU frame, the Detail and Presence drags' included, dissolved from that frame's draft revision and boundary, ending after 159 to 179 ms at the next message; the next gesture's first tick cancelled a release's dissolve 59 and 61 ms in over two runs, and turning the clipping overlays off cancelled another 66 and 78 ms in; and an idle second after the release's and the stroke's dissolves drew one frame and ran one update, the window's own.

**Across the corpus.** The harness's GPU frame for each recipe against the frame settlement presented, which the dissolve ended on (the since deleted `gpu_colour_corpus_at_fit`, `gpu_mask_corpus_at_fit`, `gpu_presence_corpus_at_fit`, the generated JPEGs, the zone plates and the Z6, X100VI and Air 2S; the `test` profile build at `61592997`):

| Families | Class | Cells within the limits | Worst mean | Worst block | Worst p99 | Worst \|ΔL\*\| |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Colour: Basic, Tone curve, Mixer, Vignette, the colour stack, crops, lens and perspective | pointwise | 62 of 62 | 0.052 | 0.25 | 0.87 | 0.002 |
| Every mask kind | pointwise | 42 of 42 | 0.025 | 0.12 | 0.84 | 0.009 |
| Presence | spatial | 49 of 49 | 0.042 | 0.49 | 0.98 | 0.010 |

Two colour cells are gaps: a lens distortion before the boundary, which the surface cannot run yet.

## GPU previews at 100%

The GPU frame of a drag at 100% ([GPU previews](../design/gpu-preview.md#at-100-and-above)) against the exact visible region the shared quiet policy settled it to on that build, on every family of the [corpus](../../fixtures/preview/corpus.json) over every source this host has, in the largest window the owner's display holds. Pixel measurements and charges, not timings. The GPU-first stage 5 deleted that region and its refinement; the GPU now draws the picture at rest over the visible region too, which the [release gate](#gpu-qualification-against-the-reference) holds to the reference renderer's frame.

### Scope

- **The view.** The M4 MacBook Pro's 3024 × 1964 display, a 1512 × 982 logical window at 2× with both panels closed, at 100% and scrolled to the photograph's centre: a 3026 × 1826 region of the output stage (`viewport_rect`, with its guard pixels), or the whole stage where it is smaller (the Presence fixture's 1440 × 960). This is the largest window the owner's display holds, and so the largest region the corpus measures; a larger window, such as an external display's, draws a larger region, whose slot may pass the budget ([after Detail, Presence](#after-detail-presence)).
- **The frames.** The CPU frame was the preview worker's exact region of that view, from the job the quiet policy asked for (`viewport` set, `Settle`), or, for a stack whose region the worker declined and rendered whole (an estimate behind an earlier spatial layer), that region of the exact whole frame the view drew it from. The GPU frame was the plan from the recipe's first pixel layer at the exact stage over the boundary the worker renders for a region job (`qualification::region_boundary`, `Render::region_boundary`: the window of the layer's received stage the region reads on the GPU, each spatial operation after the layer by its halo alone, with the exact region's whole-stage estimates), converted with the region (`surface_plan_over`), a lens warp's grid over the region at 100%, and drawn by the photo surface's own shader, read back headlessly. Both are compared at the region's size. A Detail or Presence layer is planned as a drag of it draws: in its GPU shape, every unit, while that slot fits the budget, and in the CPU's shape, the units its values need, when only that one does.
- **What a Detail frame is judged against.** The exact visible region, the frame that replaced the GPU frame at a percentage zoom on that build. The owner's decision to judge a Detail drag against the CPU's moving proxy applied to Fit, where the frame that settled came from the exact render and the moving frame the GPU stood in for was a proxy; at 100% the CPU's own frame is the exact region, so there is nothing between them.
- **Gaps.** A slot the surface would charge past the GPU-preview budget, or a boundary past the 256 MiB bound on one, was the CPU path and named `budget-exceeded`, as the desktop named it. A stack with Dehaze behind Detail is measured against the exact whole frame's region, the frame the view draws. These figures were measured with Dehaze's light read from the estimate store the exact render filled, the exact stage's; a region plan now reads the light its light link computes from the whole stage, the same light within 2.9 × 10⁻⁶ for a prefix of colour alone ([design](../design/gpu-preview.md#spatial-programs)). A mask that selects nothing inside the region is a gap, as at Fit.
- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal; the `test` profile build of `0c98e791`'s chain of links with a region's frame held in its own picture's bucket, 2026-10-03, `gpu_preview_corpus_at_100_percent`, under the 2 GiB budget and the 256 MiB bound on a boundary, reading the store, with Detail's half-precision planes ([plane precision](#plane-precision)), each spatial operation a link of its own. One run; the pixels are deterministic.
- **Sources.** As [at Fit](#gpu-colour-programs-at-fit): the generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture, and the Z6, X100VI and Air 2S RAWs through the private RAW manifest.

### Results

Worst of each statistic over a recipe's measured cells, what their slots charge, and its gaps. All 247 measured cells meet their class's limits, the 49 Presence cells and 63 Detail cells among them, none past half of any limit, and no slot passes the budget; `cargo xtask preview-error` over each pair is in the run's `commands.sh`.

| Recipe | Measured | Mean | Worst block | p99 | \|ΔL\*\| | Max | Charged | Gaps |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| basic-full-jpeg | 4 | 0.010 | 0.16 | 0.68 | 0.002 | 0.84 | 17.1–67.0 MB | — |
| basic-full-raw | 3 | 0.035 | 0.21 | 0.66 | 0.000 | 1.96 | 111.2–202.9 MB | — |
| tone-curve | 7 | 0.038 | 0.21 | 0.81 | 0.002 | 1.84 | 17.1–202.9 MB | — |
| mixer | 7 | 0.038 | 0.20 | 0.80 | 0.000 | 1.82 | 17.1–202.9 MB | — |
| vignette | 7 | 0.005 | 0.15 | 0.22 | 0.001 | 1.13 | 17.1–111.2 MB | — |
| colour-stack-jpeg | 4 | 0.014 | 0.25 | 0.59 | 0.001 | 1.17 | 17.1–67.0 MB | — |
| colour-stack-raw | 3 | 0.039 | 0.21 | 0.73 | 0.000 | 1.91 | 111.2–202.9 MB | — |
| mask-linear | 7 | 0.038 | 0.20 | 0.76 | 0.004 | 1.76 | 17.1–202.9 MB | — |
| mask-radial | 6 | 0.037 | 0.22 | 0.78 | 0.001 | 1.72 | 17.1–202.9 MB | 1 the mask selects nothing on this source |
| mask-brush | 5 | 0.038 | 0.22 | 0.78 | 0.001 | 1.79 | 17.1–202.9 MB | 2 the mask selects nothing on this source |
| mask-luminance-range | 7 | 0.042 | 0.22 | 0.76 | 0.009 | 1.76 | 17.1–202.9 MB | — |
| mask-colour-range | 7 | 0.047 | 0.25 | 0.78 | 0.009 | 1.78 | 17.1–202.9 MB | — |
| mask-composed | 7 | 0.038 | 0.20 | 0.77 | 0.004 | 1.76 | 17.1–202.9 MB | — |
| crop-straightened | 7 | 0.041 | 0.23 | 0.80 | 0.001 | 1.85 | 20.3–251.6 MB | — |
| crop-rotated | 7 | 0.043 | 0.22 | 0.82 | 0.001 | 1.75 | 12.9–401.4 MB | — |
| crop-straightened-tight | 7 | 0.042 | 0.15 | 0.85 | 0.001 | 1.61 | 1.3–80.2 MB | — |
| perspective-warp | 4 | 0.003 | 0.07 | 0.16 | 0.001 | 0.40 | 21.7–83.3 MB | — |
| lens-perspective-warp | 3 | 0.032 | 0.19 | 0.72 | 0.000 | 1.73 | 176.2–187.1 MB | — |
| lens-perspective-warp-jpeg | 1 | 0.051 | 0.23 | 0.39 | 0.001 | 1.76 | 85.6–85.6 MB | — |
| presence-texture | 7 | 0.049 | 0.20 | 0.88 | 0.001 | 1.89 | 62.1–391.4 MB | — |
| presence-clarity | 7 | 0.059 | 0.29 | 0.91 | 0.008 | 1.78 | 62.1–510.4 MB | — |
| presence-dehaze | 7 | 0.049 | 0.29 | 0.81 | 0.002 | 1.96 | 62.1–429.7 MB | — |
| presence-texture-clarity | 7 | 0.078 | 0.26 | 0.95 | 0.002 | 1.86 | 62.1–516.3 MB | — |
| presence-all | 7 | 0.080 | 0.49 | 0.90 | 0.003 | 8.14 | 62.1–566.4 MB | — |
| presence-negative | 7 | 0.004 | 0.07 | 0.00 | 0.000 | 1.36 | 62.1–566.4 MB | — |
| presence-all-masked | 7 | 0.047 | 0.49 | 0.83 | 0.002 | 6.49 | 62.1–566.4 MB | — |
| crop-dehaze | 8 | 0.051 | 0.30 | 0.82 | 0.002 | 2.19 | 65.9–541.7 MB | — |
| crop-dehaze-negative | 8 | 0.008 | 0.11 | 0.48 | 0.000 | 1.34 | 65.9–541.7 MB | — |
| crop-presence-all | 8 | 0.084 | 0.51 | 0.90 | 0.004 | 3.70 | 65.9–757.2 MB | — |
| crop-basic-under-dehaze | 8 | 0.058 | 0.40 | 0.82 | 0.049 | 2.25 | 37.6–441.2 MB | — |
| detail-moderate | 7 | 0.030 | 0.22 | 0.76 | 0.000 | 1.60 | 83.5–500.8 MB | — |
| detail-noise-stress | 7 | 0.020 | 0.16 | 0.68 | 0.001 | 1.46 | 83.5–497.6 MB | — |
| detail-sharpen-stress | 7 | 0.049 | 0.24 | 0.86 | 0.001 | 1.79 | 83.5–480.1 MB | — |
| detail-moderate-masked | 7 | 0.034 | 0.22 | 0.78 | 0.000 | 1.69 | 83.5–500.8 MB | — |
| detail-noise-stress-masked | 7 | 0.031 | 0.22 | 0.75 | 0.000 | 1.73 | 83.5–497.6 MB | — |
| detail-sharpen-stress-masked | 7 | 0.039 | 0.22 | 0.81 | 0.000 | 1.81 | 83.5–480.1 MB | — |
| detail-presence | 7 | 0.052 | 0.26 | 0.90 | 0.001 | 1.41 | 139.5–1,159.2 MB | — |
| detail-presence-local | 7 | 0.049 | 0.25 | 0.90 | 0.001 | 1.65 | 135.3–1,032.3 MB | — |
| detail-presence-masked | 7 | 0.035 | 0.22 | 0.80 | 0.002 | 1.60 | 139.5–1,159.2 MB | — |

- **Pointwise and geometry.** The largest figures are a mean of 0.051 (a lens warp over the zone plate with a lens identity), a worst block of 0.25 (the Air 2S's colour range), a p99 of 0.85 (the Air 2S's tight straightened crop) and a |ΔL\*| of 0.009 (the range masks), against limits of 0.5, 1.0, 2.0 and 0.25. The RAW cells through a lens warp, a straightened crop or a perspective warp are drawn by the tail from an `rgba32float` intermediate that keeps their linear values unquantized: their 12 cells carry a signed ΔL\* within ±0.0001 and a p99 of 0.85 at most, where an `rgba16float` intermediate written by the M4's own conversion toward zero carried −0.004 to −0.007 and up to 0.96 ([plane precision](#plane-precision)). A perspective warp's homography leaves a worst block of 0.07 at most, 0.02 on the zone plate, and a lens warp's grid 0.23 at most, on the zone plate with a lens identity. Without a warp the JPEGs' frames are the CPU's to within a fraction of a code, as at Fit. A RAW's vignette, a finishing layer after its lens warp, is measured here: the worker's region boundary held the warped frame it received.
- **The 60 MP JPEG in the largest window.** Its slots charge 67.0 MB for a colour or masked stack, 82.6 MB through a perspective warp, 107.6 MB through the straightened crop, and for Presence, every unit held whichever fields are moved, 252.5 MB for Texture, 290.9 MB for Dehaze, 393.0 MB for Clarity, 400.1 MB for Texture and Clarity and 456.0 MB for all three fields, masked or not: each window is the region grown by the halos alone, with no tile grid. Detail's slots, every unit, charge 337.1 to 351.5 MB; Detail then Texture and Clarity 933.5 MB and Detail then all three fields 1,099.1 MB, each operation a link of its own with its own planes and the intermediate Detail's link writes between them ([after Detail, Presence](#after-detail-presence)). Each holds its frame in the bucket of the region's own picture, 3072 × 1856 texels, where the photograph's square bucket of 3072 × 3072 charged every one of them 14.9 MB more.
- **The RAWs.** A colour, masked or warp slot charges 111.2 to 251.6 MB, a rotated crop's up to 401.4 MB, a geometry tail's `rgba32float` intermediate among them, and Presence's, every unit held, 295.9 to 391.4 MB for Texture, 333.6 to 429.7 MB for Dehaze, 423.9 to 510.4 MB for Clarity, 429.8 to 516.3 MB for Texture and Clarity and 481.3 to 566.4 MB for all three fields, masked or not, all measured in the GPU's shape. Detail's slots charge 382.1 to 500.8 MB, every unit, Detail then Texture and Clarity 986.1 to 1,032.3 MB and Detail then all three fields 1,114.3 to 1,159.2 MB, the peak of the measured cells.
- **Detail.** Every measured cell is within a fraction of the spatial limits, its half-precision planes included: a worst block of 0.26, a p99 of 0.90 and a mean of 0.052, each on the Air 2S, Detail then all three fields, against 2.5, 5.0 and 1.0, and on the JPEGs and the zone plate a worst block of 0.12 at most. The exact visible region it was judged against was the frame that replaced it, so these were the jump a person saw at settle too.
- **Dehaze.** With its light taken on the GPU from the region alone, the Z6's frame missed the spatial limits (worst block 13.14, p99 2.87, mean 0.76) and the X100VI's (worst block 3.20): the region's brightest blocks are not the whole stage's, and the exact region reads the whole stage's light. With the whole stage's light every Dehaze cell meets the spatial limits (worst block 0.29 and p99 0.81 on the Air 2S), which a region plan's light link computes ([lights taken over less than the whole stage](#lights-taken-over-less-than-the-whole-stage)).
- **Presence drags at 100%.** In the `gpu-preview-zoom` scenario, over a committed Dehaze and Clarity at 100%, each GPU tick of a Texture drag runs 5 compute passes and of a Clarity drag none (`gpu_preview_spatial_passes`), the first draw over a new boundary running every pass. These figures predate the per-frame light, under which Presence runs 20 passes, its light link computing the light beside them; the scenario now draws a Basic drag under that Presence on the GPU, its light computed every tick, and with Detail committed under Presence and Dehaze a Texture and a Detail drag reading the light the picture at rest computed from Detail's output, which has not been run since ([GPU previews](../design/gpu-preview.md#at-100-and-above)).

### Reproducing it

The harness that measured these figures, `gpu_preview_corpus_at_100_percent`, is deleted with the CPU region it compared against. The release gate runs every family at 100% over the visible region of the owner's largest window against the reference renderer's frame, not the CPU region, so it reproduces the comparison under today's contract, not these figures; `--recipes` measures only the recipes it names. A selection, so the run reports itself incomplete:

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom 100 --kind picture-at-rest,picture-in-motion
```

### Lights taken over less than the whole stage

A lesson the per-frame light rests on ([design](../design/gpu-preview.md#spatial-programs)). Before the light link, a drag's plan read Dehaze's light from the estimate store or took it on the GPU from the stage it held, and five other lights were measured against the exact frame a drag settles to, over the sources the corpus measures Detail beside Presence on at 100% and behind the corpus's straightened crop at Fit (Apple M4 Pro, Metal, 2026-10-05; the measurements were retired with the store-reading plans). Each missed the spatial limits where its stage was not the whole exact one:

- **A proxy's own light**, the drafted stack's Fit proxy or the CPU's moving frame: their 16 px blocks cover more of the picture than the exact stage's, missing a Presence drag by up to a mean of 8.78 (the Fit proxy's) and 5.34 (the moving frame's).
- **A window's light**: over a windowed Fit proxy behind a straightened crop, the light of the window or of the whole proxy stage missed by a mean of 2.1 to 17.9 on the 60 MP JPEG; over a 100% region, the region's own light missed on the Z6 and X100VI.
- **The light of the stack a drag started from**, held for the drag: within the limits for a Detail drag while Dehaze removes a veil (35 of 35 cells), but at Dehaze −100 sharpen stress missed on the zone plate (signed ΔL\* −1.15) and the Air 2S (+2.34), and under a Basic drag it missed on 25 of 49 cells, at three stops of exposure worst (a mean of 60.9).
- **A light from a held reduced stage**, the drafted colour run over Dehaze's own 16 px reduction of the dragged layer's input: 44 of 49 Basic cells, missing at three stops under every field at ±100 (a mean of up to 1.90, a signed ΔL\* of −1.67), and behind the crop at ±3 EV on the 60 MP JPEG. The [reduction factor](#the-per-frame-lights-reduction-factor) measured the same gap at every coarser factor.

The exact stage's light was within a mean of 0.065 and a worst block of 0.54 on every cell. So every light is computed from the whole content stage at full resolution.

### A boundary's arrival

What a boundary's arrival holds at once — the desktop's copy, wgpu's upload staging and the texture — with the upload spread at `UPLOAD_PER_FRAME` and, as before it was spread, written in one frame. A functional measurement, not a timing: `a_boundary_arrival_measured` hands the photo surface the largest `f32` boundary a RAW region with Clarity's margin reads in the largest window, 4705 × 2817 texels (212.1 MB), frame by frame until the surface holds it, then lets its copy go as the desktop does, recording each frame's staged bytes, the slot's charge and the process's footprint, and the process's own peak footprint, which catches what falls between frames. Apple M4 Pro on Metal, the `test` profile build at `b1c6ff64`, each mode in its own process, one run each.

| | Frames | Staged a frame | Footprint during the upload | At the frame that draws it | Process peak | After the copy is let go |
| --- | ---: | --- | --- | --- | --- | --- |
| Spread, 32 MiB a frame | 7 | 33.6 MB, the last 10.6 MB | +463.1 to +471.8 MB | +576.1 MB | +576.3 MB | +565.5 MB |
| Whole, as before | 1 | 212.1 MB | — | +778.1 MB | +778.3 MB | +769.9 MB |

The footprint is above the process's before the boundary, which holding the copy alone raises by 212.2 MB. The slot charges 316.9 MB in both: the boundary's texture and the output's, 104.8 MB in its size bucket, which the drawing frame writes, a full-size output this measurement draws where a region plan's is the region's. Without that output, the arrival held 2.2 times the boundary spread (the copy, the texture and a frame's staging, at most 471.8 MB) where it held 3.1 times written whole (665 MB). After the arrival the footprint stays near its peak in both: the system allocator keeps the copy's freed pages for its next block of that size (the measurement holds the copy as the only reference when it lets it go), and written whole, the 212 MB of staging stays too, 204 MB more than spread.

```sh
cargo test -p luxforge-ui --lib a_boundary_arrival_measured -- --ignored --nocapture
LUXFORGE_ARRIVAL=whole cargo test -p luxforge-ui --lib a_boundary_arrival_measured -- --ignored --nocapture
```

## Painting over masked spatial layers

Brush strokes over masks that hold Clarity and Texture, drawn through the GPU preview's chain of links, its scratch pool, its masked passes and its incremental ticks ([design](../design/gpu-preview.md#where-the-code-lives)). Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2 (25F84), the `Apple M4 Pro` adapter on Metal, 2026-10-04 unless a row says otherwise, on a host shared with other sessions, with background applications running: the load column is the one-minute load at each run's start, and a run above the 8.0 threshold is marked. Megabytes are 10⁶ bytes. The builds:

- **Before**: the release build of `61bca5ac` (binary SHA-256 `a4fc2df6…`), where each link holds its own scratch planes, Texture's band takes a two-channel plane and a recipe holds four masked spatial layers.
- **After**: the release build of `fdfd267d` (binary SHA-256 `2990e3ec…`), that base with the scratch pool, the one charge, the one-channel band, the cap of 16 with its compile cache and warm list, and the fallback notice, without the efficiency work merged beside them.
- **Main**: local main at `946751cc`, the same changes with the [efficiency](../design/efficiency.md) work, for the memory, the 100% window, the harness layout's tick cost and the corpus.

### A stroke's latency

`editor-latency --mode paint --samples 120 --window 1728x1080`: a full-screen window on the 3456 × 2160 display at 2×, whose Fit stage of the generated 24 MP JPEG is 2292 × 1528. 120 positions of the curved stroke, one every 24 ms, on the brushed mask (size 0.06, feather 50), which holds a masked exposure of +0.6 EV and, with `--mask-presence`, a masked Presence layer of Clarity 50 and Texture 40; `--masks N` adds N − 1 radial masks across the frame holding the same, and the recipe holds every masked Basic layer first, then every masked Presence layer. `--zoom 100` paints at 100%, and the X100VI runs, through the private RAW manifest, add a global Detail layer (`--detail`). Milliseconds, nearest rank, from each position's input to the frame that shows it (`position_to_presented_frame`); *early* and *late* are the stroke's first and last 30 positions; the GPU-preview peak is `gpu_preview_peak_bytes` over the run and the scratch pool `last_gpu_preview_scratch_bytes`. The three-mask rows ran before and after back to back, then in reverse order. The before build gives Presence to four masks at most, so the 10- and 16-mask rows are new workloads, measured after only.

| Run | Binary | Load | Frames | Position to frame p50 / p95 / max | Early p95 | Late p95 | Press to first frame | GPU-preview peak | Scratch pool |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Fit, 3 masks, before 1 | `a4fc2df6` | 2.00 | 120 GPU | 7.9 / 8.9 / 9.2 | 8.8 | 9.0 | 1.5 | 448 MB | — |
| Fit, 3 masks, after 1 | `2990e3ec` | 3.03 | 120 GPU | 7.8 / 8.5 / 8.9 | 8.5 | 8.7 | 1.5 | 257 MB | 74 MB |
| Fit, 3 masks, after 2 | `2990e3ec` | 4.39 | 120 GPU | 7.9 / 8.9 / 9.3 | 8.8 | 9.0 | 1.3 | 257 MB | 74 MB |
| Fit, 3 masks, before 2 | `a4fc2df6` | 4.64 | 120 GPU | 7.9 / 8.8 / 9.2 | 8.8 | 8.9 | 1.4 | 448 MB | — |
| 100%, 3 masks, before 1 | `a4fc2df6` | 5.27 | 118 GPU, 2 CPU (`boundary-pending`, `boundary-uploading`) | 7.6 / 9.0 / 77.9 | 51.8 | 9.0 | 77.9 | 973 MB | — |
| 100%, 3 masks, after 1 | `2990e3ec` | 5.01 | 118 GPU, 2 CPU (the same) | 7.8 / 8.9 / 73.6 | 49.9 | 8.2 | 73.6 | 542 MB | 168 MB |
| 100%, 3 masks, after 2 | `2990e3ec` | 4.88 | 119 GPU, 1 CPU (`boundary-pending`) | 7.6 / 9.3 / 60.9 | 24.6 | 9.0 | 60.9 | 542 MB | 168 MB |
| 100%, 3 masks, before 2 | `a4fc2df6` | 5.84 | 118 GPU, 2 CPU (`boundary-pending`, `boundary-uploading`) | 7.7 / 9.6 / 83.8 | 59.1 | 8.9 | 83.8 | 973 MB | — |
| X100VI at Fit, Detail and 3 masks, before 1 | `a4fc2df6` | 5.09 | 120 GPU | 7.8 / 8.7 / 21.5 | 8.7 | 9.0 | 1.6 | 728 MB | — |
| X100VI at Fit, Detail and 3 masks, after 1 | `2990e3ec` | 6.40 | 120 GPU | 7.9 / 8.9 / 10.4 | 8.9 | 9.0 | 10.4 | 557 MB | 186 MB |
| X100VI at Fit, Detail and 3 masks, after 2 | `2990e3ec` | 6.52 | 120 GPU | 7.7 / 8.8 / 10.8 | 8.5 | 8.8 | 1.7 | 557 MB | 186 MB |
| X100VI at Fit, Detail and 3 masks, before 2 | `a4fc2df6` | 6.78 | 120 GPU | 7.8 / 8.9 / 9.8 | 8.9 | 8.4 | 3.7 | 728 MB | — |
| Fit, 10 masks, after 1 (new workload) | `2990e3ec` | 7.61 | 120 GPU | 8.3 / 23.4 / 25.2 | 23.9 | 9.3 | 11.8 | 558 MB | 74 MB |
| Fit, 10 masks, after 2 (new workload) | `2990e3ec` | 8.44 (above 8.0) | 120 GPU | 8.3 / 22.8 / 23.7 | 22.8 | 10.9 | 2.6 | 558 MB | 74 MB |
| Fit, 16 masks, after 1 (new workload) | `2990e3ec` | 10.61 (above 8.0) | 120 GPU | 9.2 / 24.4 / 36.5 | 24.0 | 24.2 | 4.0 | 815 MB | 74 MB |
| Fit, 16 masks, after 2 (new workload) | `2990e3ec` | 13.99 (above 8.0) | 120 GPU | 8.8 / 24.3 / 34.4 | 9.6 | 11.4 | 4.2 | 815 MB | 74 MB |
| 100%, 10 masks, after 1 (new workload) | `2990e3ec` | 8.29 (above 8.0) | 120 CPU (`budget-exceeded`) | 548.5 / 733.5 / 821.2 | 800.7 | 498.6 | 478.2 | the Fit slot's 558 MB | — |
| 100%, 10 masks, after 2 (new workload) | `2990e3ec` | 8.01 (above 8.0) | 120 CPU (`budget-exceeded`) | 548.7 / 750.7 / 849.8 | 823.3 | 482.8 | 495.6 | the Fit slot's 558 MB | — |
| 100%, 16 masks, after 1 (new workload) | `2990e3ec` | 12.22 (above 8.0) | 120 CPU (`budget-exceeded`), 89 positions shown | 1013.8 / 1339.1 / 1424.7 | 1399.8 | — | 818.3 | the Fit slot's 815 MB | — |
| 100%, 16 masks, after 2 (new workload) | `2990e3ec` | 12.28 (above 8.0) | 120 CPU (`budget-exceeded`), 92 positions shown | 1002.5 / 1479.2 / 1571.1 | 1549.4 | 667.1 | 877.6 | the Fit slot's 815 MB | — |

Earlier rows, on the release editor built from the working tree over `5dccacc6` (binary SHA-256 `3a5b89c8…`), 2026-10-03, where each link held its own scratch planes, so their GPU-preview peaks are of that layout. `--presence` adds a global Presence layer (Texture 25, Clarity 20) under the masks:

| Case | Load | Frames | Input to frame p50 / p95 / max | Early p95 | Late p95 | Press to first frame | GPU-preview peak |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| Fit, 3 masks, preview off | 1.56 | 21 CPU shown, 99 superseded | 145.1 / 157.5 / 157.7 | 248.3 | 277.0 | 127.4 | — |
| Fit, 3 masks over a global Presence layer | 4.74 | 120 GPU | 7.8 / 9.0 / 9.3 | 9.1 | 8.8 | 1.7 | 693 MB |
| 100%, 3 masks, overlay shown | 8.82 (above 8.0) | 118 GPU, 2 CPU (`boundary-pending`, `boundary-uploading`) | 7.9 / 8.9 / 231.1 | 52.4 | 8.6 | 78.1 | 980 MB |
| 100%, 3 masks, preview off | 7.37 | 25 CPU shown, 95 superseded | 127.4 / 166.3 / 185.1 | 207.9 | 208.9 | 120.2 | — |

- **Three masks, within a frame.** Before and after compare like for like, and the latency is unchanged within noise in both orders: p95 8.5 to 8.9 ms after against 8.8 to 8.9 before at Fit, 8.9 to 9.3 against 9.0 to 9.6 at 100%, and 8.8 to 8.9 against 8.7 to 8.9 on the X100VI with Detail. Every position at Fit is drawn on the GPU at the redraw after its input, the press 1.3 to 10.4 ms from its input to its frame where the stack's resident boundary is held ([the GPU source](../design/gpu-preview.md#the-gpu-source)).
- **The GPU-preview peak falls** wherever two or more spatial layers are held: from 448 to 257 MB at Fit (−43%), from 973 to 542 MB at 100% (−44%), and from 728 to 557 MB on the X100VI with Detail (−23%), where Detail's half-precision planes share no texture class with Presence's, so the pool holds both links' scratch, 186 MB.
- **10 and 16 masks at Fit**, new workloads: every position is drawn on the GPU, but p95 is 22.8 to 24.4 ms, **missing the 16 ms target**, because a tick's GPU work passes the 120 Hz frame ([what a tick costs](#what-a-tick-costs-the-gpu)). The loads above 8.0 on most of these runs came largely from the runs' own settled CPU renders of 10 to 16 masked layers ([the settled render](#the-masked-spatial-primitive-one-to-sixteen-layers)). The pool stays 74 MB, one link's scratch, at every count.
- **10 and 16 masks at 100%**, new workloads: every tick took the CPU path naming `budget-exceeded`, the desktop's estimate refusing the slot before any boundary was asked for, correctly, since the window grows with every chained link ([the window](#the-100-window-grows-with-the-chain)). A drag at 100% whose region slot passes the budget is now drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging", `budget-reduced`), and is sharp at rest. The status bar's notice for `budget-exceeded` was `GPU memory full`. The harness records no notice per tick; the notice for each CPU reason is the status model's, `GPU memory full` for `budget-exceeded` and none for `boundary-pending` or `boundary-uploading`. The peak column is the Fit slot the run opened with.
- **The first stroke after a zoom.** At 100% with three masks the first one or two ticks were the CPU's while the region's boundary rendered and uploaded: the press reached the screen in 61 to 84 ms and the early positions' p95 was 25 to 59 ms. Once the boundary was held every tick was the GPU's, late p95 8.2 to 9.0 ms, and 8.6 ms with the mask overlay shown, its region coverage laid over the GPU region frame. The harness zoomed over a retained exact frame just before it painted, so no view job asked for the region's resident boundary first ([later](../design/gpu-preview.md#later)).
- **With the preview off** the CPU path showed 21 of the 120 positions at Fit and 25 at 100%; counting each position by the frame that carried it, 197.4 / 255.3 / 277.0 ms at Fit and 174.8 / 240.9 / 301.5 at 100%.

### What a tick costs the GPU

`gpu_mask_a_painted_stroke_costs_where_it_changes` (release, `--ignored --nocapture`, the qualifier's headless device on the same adapter): the same stroke's 119 ticks over a brushed mask and two radials, each holding a masked Basic and a masked Presence layer, every tick after the first submitted without waiting, so the figure is the GPU's throughput a tick. The host's decimation moves some of the stroke's kept positions at each tick, so a tick's change covers 6.3% of the Fit stage and 7.1% of the region on average. Before (the base tree's test over its build) and after, back to back then reversed, at loads of 2.2 to 2.5:

| Run | Fit, 2292 × 1528: incremental / whole, a tick | 100% region, 2994 × 2642: incremental / whole, a tick |
| --- | ---: | ---: |
| before 1 | 2.44 / 6.70 ms | 4.46 / 14.91 ms |
| after 1 | 2.37 / 6.55 ms | 4.31 / 14.66 ms |
| after 2 | 2.34 / 6.57 ms | 4.33 / 14.61 ms |
| before 2 | 2.44 / 6.73 ms | 4.46 / 14.99 ms |

Encoding takes 0.06 to 0.10 ms a tick in every run. The pool leaves a tick's GPU cost unchanged within noise, after about 3% lower in both orders. The incremental ticks take 2.7 to 3.4 times less GPU time than whole ones and draw what whole ones do, bit for bit (`gpu_mask_a_painted_stroke_is_evaluated_where_each_tick_changes_it`). On the earlier `5dccacc6` build, with the driver's shader cache warm from earlier runs, the compile and first two ticks took 71 ms at Fit and 80 ms over the region.

**The paint harness's layout at Fit** (`gpu_mask_the_paint_harness_layout_costs_a_tick_at_fit`, release, on main with the test, load about 4.6 to 4.9): the 2292 × 1528 Fit stage of a 6000 × 4000 photograph, the masked Basic layers then the masked Presence layers, the stroke on the first mask, so every Presence link after it is evaluated incrementally. The slot charges 257.4, 557.7 and 815.2 MB, the paint runs' peaks to the MB.

| Masks | Incremental, a tick (encoding) | Whole, a tick (encoding) | One tick alone p50 / p95 / largest |
| ---: | ---: | ---: | ---: |
| 3 | 2.04 ms (0.10) | 6.88 ms (0.06) | 2.77 / 5.30 / 6.64 ms |
| 10 | 8.80 ms (0.21) | 17.25 ms (0.13) | 10.80 / 14.49 / 15.75 ms |
| 16 | 14.46 ms (0.25) | 26.40 ms (0.20) | 17.32 / 21.06 / 23.76 ms |

At 10 and 16 masks a tick's GPU work passes the 8.33 ms frame, which is the paint runs' 23 to 24 ms p95. The pool does not add to it (above): it is the work of 10 to 16 chained Presence links, each evaluated incrementally around a change that grows link by link with each link's reach ([later](../design/gpu-preview.md#later)).

### The scratch pool's memory

What a masked Presence layer costs the GPU-preview budget with every link's scratch planes in the slot's one pool and Texture's band in one channel ([design](../design/gpu-preview.md#plane-sharing-and-precision)). Deterministic, one run: `cargo test --release -p luxforge-app --bin luxforge gpu_shared_scratch_measured -- --ignored --nocapture` on main, the slot's charge (`Qualifier::charged_bytes`, which the live slot's `gpu_preview_in_use_bytes` equals) for 1, 2, 4, 8 and 16 masked Presence layers, each through a radial of its own. At Fit, a 2292 × 1528 boundary against a 6000 × 4000 stage; at 100%, a fixed 3778 × 2578 window at (1111, 711) of the 6000 × 4000 exact stage, the region drawn being the window itself. A JPEG's boundary is `rgba16float` (the byte path); a RAW's is `rgba32float` (the linear path), as are its intermediates. Megabytes, against the 2 GiB budget of 2,147.5 MB:

| Layers of | Stage | Boundary | 1 layer | 2 | 4 | 8 | 16 | Each layer after the first | Layers within 2 GiB |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Texture and Clarity | Fit | JPEG | 143.5 | 186.4 | 272.3 | 443.9 | 787.1 | 42.9 | 47 |
| Texture and Clarity | Fit | RAW | 171.6 | 242.5 | 384.3 | 668.0 | 1,235.4 | 70.9 | 28 |
| Texture and Clarity | 100% window | JPEG | 365.3 | 484.6 | 723.3 | 1,200.6 | 2,155.2 | 119.3 | 15 |
| Texture and Clarity | 100% window | RAW | 443.2 | 640.5 | 1,034.9 | 1,823.9 | 3,401.9 | 197.2 | 9 |
| Texture, Clarity and Dehaze | Fit | JPEG | 154.3 | 198.9 | 288.3 | 466.9 | 824.2 | 44.7 | 45 |
| Texture, Clarity and Dehaze | Fit | RAW | 182.3 | 255.0 | 400.3 | 691.0 | 1,272.5 | 72.7 | 28 |
| Texture, Clarity and Dehaze | 100% window | JPEG | 395.3 | 519.5 | 767.9 | 1,264.8 | 2,258.5 | 124.2 | 15 |
| Texture, Clarity and Dehaze | 100% window | RAW | 473.2 | 675.3 | 1,079.6 | 1,888.1 | 3,505.2 | 202.1 | 9 |

Each layer after the first adds its intermediate, its kept planes and parameters and 2,048 bytes of buffers; the pool holds exactly one link's scratch:

| Layers of | Stage | Boundary | Boundary texture | Output and uniform | Intermediate | Kept planes and parameters | Pool, one link's scratch |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| Texture and Clarity | Fit | JPEG | 28.0 | 26.2 | 28.0 | 14.9 | 74.4 |
| Texture and Clarity | Fit | RAW | 56.0 | 26.2 | 56.0 | 14.9 | 74.4 |
| Texture and Clarity | 100% window | JPEG | 77.9 | 39.0 | 77.9 | 41.4 | 207.0 |
| Texture and Clarity | 100% window | RAW | 155.8 | 39.0 | 155.8 | 41.4 | 207.0 |
| Texture, Clarity and Dehaze | Fit | JPEG | 28.0 | 26.2 | 28.0 | 16.6 | 83.4 |
| Texture, Clarity and Dehaze | Fit | RAW | 56.0 | 26.2 | 56.0 | 16.6 | 83.4 |
| Texture, Clarity and Dehaze | 100% window | JPEG | 77.9 | 39.0 | 77.9 | 46.3 | 232.1 |
| Texture, Clarity and Dehaze | 100% window | RAW | 155.8 | 39.0 | 155.8 | 46.3 | 232.1 |

The same plans with each link holding its own scratch, the layout without the pool, with the band in one channel:

| Layers of | Stage | Boundary | Each layer after the first | Layers within 2 GiB | The pool saves at 16 layers |
| --- | --- | --- | ---: | ---: | ---: |
| Texture and Clarity | Fit | JPEG | 117.3 | 18 | 1,116.3 |
| Texture and Clarity | Fit | RAW | 145.3 | 14 | 1,116.3 |
| Texture and Clarity | 100% window | JPEG | 326.3 | 6 | 3,105.2 |
| Texture and Clarity | 100% window | RAW | 404.3 | 5 | 3,105.2 |
| Texture, Clarity and Dehaze | Fit | JPEG | 128.1 | 16 | 1,251.0 |
| Texture, Clarity and Dehaze | Fit | RAW | 156.1 | 13 | 1,251.0 |
| Texture, Clarity and Dehaze | 100% window | JPEG | 356.3 | 5 | 3,481.1 |
| Texture, Clarity and Dehaze | 100% window | RAW | 434.2 | 4 | 3,481.1 |

- **What the pool saves** is every layer's scratch after the first: 74.4 MB a layer at Fit and 207.0 MB over the window for Texture and Clarity. A further layer costs about a third of what it costs with each link's scratch its own at Fit on a JPEG, and about half on a RAW; the budget holds 47 layers of Texture and Clarity at Fit on a JPEG against 18, and 15 over the window against 6.
- **The one-channel band** keeps 4 bytes a pixel less of every Texture layer than its two-channel plane did: 14.0 MB at Fit and 39.0 MB over the window, in the first layer's charge and in every increment.
- **The fixed window understates a real chain at 100%**, since the window grows with every chained spatial layer, below.

#### The 100% window grows with the chain

`gpu_window_the_paint_harness_masks_at_100_grow_the_window_by_every_links_halo` and `gpu_window_the_paint_harness_masks_at_100_fit_the_budget_up_to_a_count` (release, ignored, on main with the tests), through the real planning path: the editor opens the generated 24 MP JPEG (6000 × 4000), another client commits the paint harness's layout, and a drag in Mask mode at 100% asks for its plan. At a percentage zoom the boundary is the view's region grown by the halo of every spatial layer chained after it (`WindowPlan::of_gpu_rect`), so each masked Presence link adds 207 px on every side (Texture 8 and Clarity 199), and every link's intermediate and kept planes and the pool are sized to that window. `plan_steps` converted every plan, and `region_charge` equals the surface's own `slot_charge` but for the links' buffers (6 to 40 KB). Budget 2,147,483,648 bytes. In the harness's view, 1728 × 1080 at 2× with the panels open, scrolled to the top left (the region 2373 × 2021 at (0, 0)):

| Masks | Window | Slot | Fits |
| ---: | --- | ---: | --- |
| 1 | 2580 × 2228 | 258.5 MB | yes |
| 2 | 2787 × 2435 | 384.7 MB | yes |
| 3 | 2994 × 2642 | 542.0 MB | yes |
| 4 | 3201 × 2849 | 733.6 MB | yes |
| 5 | 3408 × 3056 | 962.5 MB | yes |
| 6 | 3615 × 3263 | 1,232.0 MB | yes |
| 7 | 3822 × 3470 | 1,545.2 MB | yes |
| 8 | 4029 × 3677 | 1,905.2 MB | yes |
| 9 | 4236 × 3884 | 2,315.1 MB | no |
| 10 | 4443 × 4000 | 2,716.9 MB | no |
| 12 | 4857 × 4000 | 3,444.2 MB | no |
| 16 | 5685 × 4000 | 5,142.2 MB | no |

- **The corpus's view**, the region 3026 × 1826 at (1487, 1087), fits up to 5 masks, 1,819.6 MB, where 6 charge 2,287.5 MB; the window reaches the whole stage at 8 masks (3,076.8 MB), and 16 charge 5,428.9 MB. Centred with the panels open (2374 × 2022 at (1813, 989)) the slot charges 301.8, 798.9, 3,662.0 and 5,426.0 MB at 1, 3, 10 and 16 masks; centred with the panels closed (3458 × 2022), 420.3, 1,041.4, 3,670.9 and 5,434.9 MB.
- **At Fit** the plan is at the view's reduced stage at every count, a 2292 × 1528 boundary with no window, which does not grow.
- **With Dehaze on the masks** the drag changes every Dehaze layer's input, so each tick computes every layer's light from the whole stage, a light link a layer charged beside the slot; the measurement's Dehaze part has not been run since.
- **No defect.** For one window shared by every link and blind to masks, this growth is what an exact region frame needs. It over-estimates in two ways, each a proposal that is not built, with its estimated effect in the [design](../design/gpu-preview.md#later): growth bounded by each link's mask, and each link's intermediate and kept planes sized to its own output's need.

### The corpus

The five corpus harnesses, release profile, with every source the corpus names, the zone plates and the three RAWs through the private manifest among them; the pixels are deterministic. Presence and Detail at Fit and every family at 100% ran on main (`946751cc`), with the scratch pool and the one-channel band, and every statistic equals the run before them; colour and masks at Fit are that earlier run's, over the release build of `313faec4` with the chain (`fc3eb65e`) merged. Worst of each statistic over the measured cells — mean ΔE00, worst 16 × 16 block, p99 and \|signed mean ΔL\*\| — against the pointwise limits (0.5, 1.0, 2.0, 0.25) or the spatial ones (1.0, 2.5, 5.0, 0.5):

| Harness | Within the limits | Pointwise | Spatial | Gaps | Heaviest slot |
| --- | ---: | ---: | ---: | --- | --- |
| Colour at Fit | 62 of 62 | 0.051 / 0.25 / 0.87 / 0.002 | — | 2: a vignette after a RAW's lens warp | — |
| Masks at Fit | 42 of 42 | 0.021 / 0.12 / 0.66 / 0.009 | — | none | — |
| Presence at Fit | 81 of 81 | — | 0.061 / 0.51 / 0.97 / 0.049 | none | `crop-presence-all` on the Z6, 245.6 MB |
| Detail at Fit | 63 of 63 | — | 0.054 / 0.26 / 0.96 / 0.019 | none | Detail then Presence on the Air 2S, 261.2 MB |
| Every family at 100% | 247 of 247 | 0.051 / 0.25 / 0.85 / 0.009 | 0.084 / 0.51 / 0.95 / 0.049 | 3: masks that select nothing in the region (`mask-radial` and `mask-brush` on the 60 MP JPEG, `mask-brush` on the Z6) | Detail then all three Presence fields on the Air 2S, 1,123.8 MB |

Under the 2 GiB budget every 100% slot draws on the GPU. The heaviest charged 1,174.1 MB in the run before: the pool leaves Detail beside Presence as it was, since their planes share no texture class, and the one-channel band lowers it.

### The performance-rules checklist

For the chain, the masked passes, the incremental ticks and the resident boundary ([rules](../engineering/performance-rules.md#review-checklist)):

- **The original** is read, hashed and decoded only as before: on that build a boundary was rendered by the preview worker from the verified prepared source.
- **Full-frame allocations.** On the GPU, each link's intermediate (the boundary's size and format), each masked spatial layer's planes and a plane of its own for each unit's result, and the resident boundary held between gestures, all within the 2 GiB GPU-preview budget, which refuses a slot past it and names the CPU path; on that build the desktop let its copy of a boundary's texels go once the surface held them. Nothing is cloned that is shared.
- **Point queries, validation and no-op checks** render no frame; on that build samples, analysis and export stayed on the CPU, byte for byte.
- **The owner thread** plans a committed stack's own plan and its warm list at its view, a settled 100% view's among them, from the compiled stack's descriptions in `O(layers × modules)`: no frame work. The interface thread compares each tick's plan with the plans it handed (`GpuPlan::changes_since`) in `O(units + components + segments)`, reading no pixel.
- **Desktop messages.** A tick drawn on the GPU sends no preview job and uploads no frame; on that build the shared quiet policy settled a draft only the GPU had drawn with one job once input paused, as after a CPU tick. A committed job asked for the resident boundary only when no held boundary had its key, and a view job asked for the region's plans only when it settled.
- **Timers, polls and subscriptions.** None added.
- **Repeated work.** A link whose input and words did not change is not run (keyed by content: its input's key, words, blocks and pipeline); a tick runs each changed link only where its change reaches; a masked spatial layer runs its passes only over its mask; the resident boundary is rendered once per stack and view rather than once per gesture.
- **`editor-performance`.** The CPU's renders are unchanged; not run for this change.
- **Exactness.** Partial and incremental evaluation are held to whole evaluation bit for bit (`gpu_presence_a_masked_layer_runs_its_passes_over_its_mask_alone`, `gpu_mask_a_painted_stroke_is_evaluated_where_each_tick_changes_it`, `gpu_mask_a_moved_radial_is_evaluated_where_its_bounds_were_and_are`, `gpu_mask_a_mask_grown_toward_the_edge_reads_its_inputs_latest_values`); a plan over another boundary or with other clipping marks is evaluated whole (`a_change_measured_over_another_boundary_is_evaluated_whole`, `a_change_is_measured_only_over_the_same_boundary_and_marks`); and on that build every GPU frame was still settled by the CPU's.

### The performance-rules checklist for the scratch pool and sixteen layers

For the slot's scratch pool, the one charge, Texture's band in one channel, the fallback notice, and the cap of 16 masked spatial layers with its compile cache and warm list ([rules](../engineering/performance-rules.md#review-checklist)):

- **The original** is read, hashed and decoded only as before; none of these changes reads a source.
- **Full-frame allocations.** On the GPU, one pool of scratch textures a slot, each the boundary's size, its blocks or a fixed size, which every link takes in turn rather than holding a set of its own; a link's kept planes and intermediate as before; and Texture's band in a one-channel `r32float` plane, 4 bytes a pixel less than its `rg32float` one. The pool is charged once to the 2 GiB GPU-preview budget, refused before anything is created past it, and retired through the surface's retirement worker, charged until the GPU is done with it. At three masks the GPU-preview peak falls by 23 to 44%, and 16 masked Presence layers charge 815 MB at Fit ([memory](#the-scratch-pools-memory)). Nothing is added on the CPU: the notice holds one phrase and tooltip, and the warm list plan descriptions.
- **Point queries, validation and no-op checks** render no frame. The cap is checked when the stack compiles, `region_charge` is arithmetic over the plan's steps converted with no boundary and no device, and the notice reads the reason the evidence already records.
- **The owner thread** plans the warm list, one drag for each distinct drafted shape within 45 link sequences, from the compiled stack's descriptions: no frame work. The interface thread fits the pool once a tick from every link's steps, in `O(links × planes)` as fitting the planes is, and encodes a tick in 0.06 to 0.25 ms on the qualifier's device over 3 to 16 masks ([what a tick costs](#what-a-tick-costs-the-gpu)); the editor's own preparation figure (`gpu_preview_frame_us`) was not recorded for these runs.
- **Desktop messages.** None added: a tick drawn on the GPU sends no preview job and uploads no frame, a slot the desktop's estimate refuses asks for no boundary, and the notice is derived in the update each tick already runs, a change of drawing path waking the desktop once as before. `asset.state` and `history.list` are unchanged.
- **Timers, polls and subscriptions.** None added. The half second before `compiling` is said is measured at ticks, so a drag that holds still keeps what it last said and the idle editor stays asleep.
- **Repeated work.** A pool texture's key is trusted only by the schedule that wrote it, so a link runs a pass again only where another link wrote its scratch since it last ran: the one count that moves is a Dehaze drag's 20 of 22 passes after another masked Presence link has run, against 19 alone. Every single-link count stands, and a painted tick's GPU cost is unchanged within noise, 2.34 to 2.37 ms against 2.44 at Fit and 4.31 to 4.33 against 4.46 over a 100% region. The compile cache keys a link's program sequence by its steps and the format it writes, bounded at 64 sequences (`PIPELINE_CACHE`), which hold the largest plan's 19 links beside a whole warm list of at most 45. Its hit rate is held by tests rather than measured: a plan of 16 masked layers of mixed shapes compiles each sequence once, then draws every tick on the GPU, and a drag of each of the 16 finds every link it draws warmed. A rebuild is a Presence link's compile, about a second on a cold shader cache ([compile cost](#gpu-preview-compile-cost)); the cache's retained bytes were not measured.
- **`editor-performance`.** The CPU's renders are unchanged; not run for these changes. The settled render the cap allows is measured: 16 whole-frame masked layers take 2.4 s at 24 MP and 6.7 s at 60 MP on main ([the masked spatial primitive](#the-masked-spatial-primitive-one-to-sixteen-layers)), and a render expected to take more than a second shows its progress.
- **Exactness.** Every frame is held bit for bit to a fresh evaluation through the slot and through the qualifier, over chained links whose ticks alternate and over 16 masked layers of mixed shapes, with the pool poisoned as well (`chained_links_take_their_scratch_from_one_pool_and_draw_what_a_fresh_slot_draws`, `gpu_presence_chained_masked_layers_dragged_in_turn_draw_what_a_whole_evaluation_does`, `gpu_presence_sixteen_masked_layers_of_mixed_shapes_draw_what_a_whole_evaluation_does`), and so is every test of partial and incremental evaluation; the one-channel band draws what two channels did, bit for bit (`gpu_presence_texture_band_in_one_channel_draws_what_two_did`); the three charges agree (`gpu_window_a_chained_masked_plan_is_held_to_the_slots_own_charge`); and every corpus cell at Fit and at 100% stays within its limits with every statistic unchanged ([the corpus](#the-corpus)).

## GPU previews qualified on the M4

The native qualification of GPU previews ([design](../design/gpu-preview.md)): the corpus error report at Fit and at 100%, the latency of each gesture with the preview on and off, memory, idle after a dissolve, the Windows and Linux checks, the performance-rules checklist and the verify tiers. Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. On these builds the GPU preview was a preference that could be turned off, and its frames were judged against the CPU proxy and region and settled to the CPU's; the GPU-first stages deleted all three, and the [release gate](#gpu-qualification-against-the-reference) now holds the GPU to the reference renderer.

### The corpus error report

Every program and stack class of the [corpus](../../fixtures/preview/corpus.json) against the CPU frame it stood in for, over every source this host has, from the code at `61592997`: at Fit, the frame settlement presented, which the dissolve ended on ([colour programs](#gpu-colour-programs-at-fit) set out the frames); at 100%, the exact visible region the quiet policy settled to, in the largest window the owner's display holds ([at 100%](#gpu-previews-at-100)). The five harnesses, deleted since with the CPU frames they compared against (`gpu_colour_corpus_at_fit`, `gpu_mask_corpus_at_fit`, `gpu_presence_corpus_at_fit`, `gpu_detail_corpus_at_fit`, `gpu_preview_corpus_at_100_percent`) ran once each in the `test` profile; the pixels are deterministic, so the host's load, 27 to 70 while they ran, does not bear on them. The sources are the generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture, and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest, all eight verified against the corpus's SHA-256 by `cargo xtask preview-corpus`. The family sections record their own runs at earlier builds, whose Z6 and Air 2S cells were drawn through a sparser lens grid and, as every RAW cell through a lens warp, crop or perspective was until the tail's intermediate was written rounded to the nearest half, a signed ΔL\* of −0.004 to −0.010 darker; this report predates the current `rgba32float` RAW geometry tail; those RAW geometry cells describe the former half-float path and do not qualify its current accuracy or memory use ([plane precision](#plane-precision)).

Worst of each statistic over a family's measured cells — mean ΔE00, worst 16 × 16 block, p99 and \|signed mean ΔL\*\| — against the pointwise limits (0.5, 1.0, 2.0, 0.25) or the spatial ones (1.0, 2.5, 5.0, 0.5). The chain is the colour stack, the four colour programs in one pass.

| Family | Class | Fit: within the limits | Fit: mean / block / p99 / \|ΔL\*\| | 100%: within the limits | 100%: mean / block / p99 / \|ΔL\*\| |
| --- | --- | ---: | ---: | ---: | ---: |
| Basic | pointwise | 7 of 7 | 0.020 / 0.16 / 0.68 / 0.002 | 7 of 7 | 0.036 / 0.21 / 0.68 / 0.002 |
| Tone curve | pointwise | 7 of 7 | 0.021 / 0.14 / 0.71 / 0.002 | 7 of 7 | 0.039 / 0.21 / 0.82 / 0.002 |
| Mixer | pointwise | 7 of 7 | 0.022 / 0.12 / 0.76 / 0.000 | 7 of 7 | 0.039 / 0.21 / 0.80 / 0.000 |
| Vignette | pointwise | 5 of 5, 2 gaps | 0.004 / 0.04 / 0.21 / 0.001 | 7 of 7 | 0.005 / 0.15 / 0.22 / 0.001 |
| Colour stack (the chain) | pointwise | 7 of 7 | 0.021 / 0.25 / 0.71 / 0.001 | 7 of 7 | 0.040 / 0.25 / 0.74 / 0.001 |
| Straightened crops | pointwise | 21 of 21 | 0.052 / 0.20 / 0.87 / 0.002 | 21 of 21 | 0.044 / 0.23 / 0.86 / 0.001 |
| Lens and perspective | pointwise | 8 of 8 | 0.026 / 0.17 / 0.71 / 0.002 | 8 of 8 | 0.051 / 0.23 / 0.73 / 0.001 |
| Mask linear | pointwise | 7 of 7 | 0.023 / 0.12 / 0.74 / 0.004 | 7 of 7 | 0.039 / 0.20 / 0.77 / 0.004 |
| Mask radial | pointwise | 7 of 7 | 0.021 / 0.12 / 0.69 / 0.000 | 6 of 6, 1 gaps | 0.039 / 0.22 / 0.79 / 0.001 |
| Mask brush | pointwise | 7 of 7 | 0.021 / 0.12 / 0.70 / 0.001 | 5 of 5, 2 gaps | 0.039 / 0.22 / 0.78 / 0.001 |
| Mask luminance range | pointwise | 7 of 7 | 0.025 / 0.12 / 0.84 / 0.009 | 7 of 7 | 0.043 / 0.21 / 0.77 / 0.009 |
| Mask colour range | pointwise | 7 of 7 | 0.024 / 0.12 / 0.82 / 0.009 | 7 of 7 | 0.048 / 0.26 / 0.78 / 0.009 |
| Mask composed | pointwise | 7 of 7 | 0.023 / 0.12 / 0.73 / 0.004 | 7 of 7 | 0.039 / 0.20 / 0.77 / 0.004 |
| Presence | spatial | 49 of 49 | 0.042 / 0.49 / 0.98 / 0.010 | 49 of 49 | 0.080 / 0.49 / 0.95 / 0.008 |
| Detail | spatial | 63 of 63 | 0.055 / 0.26 / 1.02 / 0.020 | 49 of 49, 14 gaps | 0.060 / 0.31 / 0.99 / 0.001 |

- **The geometry tail over the zone plates.** All 216 measured cells at Fit and 201 at 100% are within their class's limits, none past half of any. A perspective warp is a homography, which the tail evaluates at every pixel in `f32`, within 0.0018 px of the CPU's map: over the zone plate, whose chirp reaches half a cycle a pixel, its worst block is 0.04 at Fit and 0.02 at 100%. Drawn through a coordinate grid that held the map within 0.041 px, it was 1.15 at Fit, past the limit. A lens warp keeps its grid, held within 0.025 output pixels at Fit and 100% as well as 0.1 display pixels: over the zone plate with a lens identity its worst block is 0.17 at Fit and 0.23 at 100%, where the display bound alone left 0.62 and 0.98. The Z6 and Air 2S cells, which every family draws through their lens profile's grid, are within a worst block of 0.26 on the pointwise families and 0.31 on the spatial ones, and of a signed ΔL\* of ±0.0002.
- **Gaps, not passes.** At Fit, the vignette after a RAW's lens warp on the Z6 and Air 2S (a lens distortion before the boundary). At 100%: a mask that selects nothing inside the region (3 cells); Detail beside all three Presence fields, masked or not, whose region the worker rendered whole while Dehaze's estimate sat behind Detail, was planned over the light that whole frame stored and measured against its region: 14 cells, each within a fraction of the spatial limits ([after Detail, Presence](#after-detail-presence)). No slot passed the 2 GiB budget.
- **Detail at settle.** Detail was judged against the CPU's moving proxy it stood in for, as the owner decided. Against the exact-derived frame Detail settled to at Fit, the GPU frame and the CPU proxy differed alike: both pass the spatial limits on 30 of 54 cells and miss them on the same 24, the zone plate's nine, the Air 2S's and those beside Presence among them (worst block 15.0 and p99 28.8 on the zone plate, 7.15 and 8.09 on a photograph). That was the jump the settle dissolve covered, the CPU path's own.

### Latency, with the GPU preview on and off

Each case's 30-input drag through `editor-latency`, the GPU preview on and, for the key cases, turned off from the palette first (that build's `--no-gpu-preview`; the preference and the flag are gone, and `--reference-renderer` now measures the reference renderer's baseline): the same build, the same host, run back to back. The release editor built from `2837c344` (binary SHA-256 `a34b2482…`), the generated 24 MP and 60 MP JPEGs and the X100VI RAF, a hidden 1440 × 900 window at 2× on the 120 Hz display, warm filesystem cache, 2026-10-02. Each run started at a one-minute load between 3.5 and 6.9 (the column), all below the 8.0 threshold; the host carried only the owner's open applications. Each input is one step left open until its frame is on screen, so every figure is one input and one frame. Milliseconds, nearest rank; a drag's 30 inputs are 30 samples, the stroke's 400 positions 400.

- **What presented means on each path.** A CPU frame is presented at its hand-off, the update in which the worker's raster became the photo surface's source (`preview_displayed`); its draw follows at the next redraw. A GPU frame has no hand-off: it is presented at the surface's first draw of the plan tagged with the input's revision, the moment that draw is encoded (`surface_frame_drawn`). The GPU figure therefore includes the wait for the next frame, which the CPU figure does not; neither is scanout. **Drawn** times both paths to that first draw, the like-for-like column; a CPU region at 100% and above had no logged draw. On the 120 Hz display the figures fall at whole frames, 8.3 ms apart.
- **Frames.** Every drag's first input took the CPU path asking for its boundary (`boundary-pending`) and its later 29 were drawn on the GPU; its p99 is that first input. With the preview off every input was the CPU's (`preference-off`).

| Case | GPU preview | Load | Frames | Presented p50 / p95 / p99 | GPU frames | CPU frames | Drawn p50 / p95 / p99 |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: |
| 24 MP JPEG at Fit, full Basic | on | 6.86 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 9.3 / 17.5 | 8.3 / 8.6 / 9.3 | 17.5 / 17.5 / 17.5 | 8.3 / 9.3 / 26.3 |
|  | on, again | 4.64 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 9.5 / 17.8 | 8.3 / 8.4 / 9.5 | 17.8 / 17.8 / 17.8 | 8.3 / 9.5 / 27.6 |
|  | off | 4.84 | 0 GPU, 30 CPU (preference-off) | 17.2 / 25.8 / 26.9 | — | 17.2 / 25.8 / 26.9 | 25.7 / 34.5 / 34.6 |
|  | off, again | 4.95 | 0 GPU, 30 CPU (preference-off) | 9.5 / 18.0 / 26.1 | — | 9.5 / 18.0 / 26.1 | 18.1 / 26.6 / 35.0 |
| 60 MP JPEG at Fit, full Basic | on | 4.99 | 29 GPU, 1 CPU (boundary-pending) | 8.4 / 8.9 / 9.3 | 8.4 / 8.7 / 8.9 | 9.3 / 9.3 / 9.3 | 8.4 / 8.9 / 17.0 |
|  | on, again | 4.23 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 8.8 / 9.5 | 8.3 / 8.7 / 8.8 | 9.5 / 9.5 / 9.5 | 8.3 / 8.8 / 16.7 |
|  | off | 4.80 | 0 GPU, 30 CPU (preference-off) | 9.1 / 16.9 / 51.0 | — | 9.1 / 16.9 / 51.0 | 17.8 / 26.0 / 55.5 |
|  | off, again | 4.42 | 0 GPU, 30 CPU (preference-off) | 9.1 / 10.1 / 17.3 | — | 9.1 / 10.1 / 17.3 | 17.5 / 18.1 / 26.1 |
| 24 MP, five exports queued | on | 4.57 | 29 GPU, 1 CPU (boundary-pending) | 8.8 / 46.0 / 129.6 | 8.8 / 39.0 / 46.0 | 129.6 / 129.6 / 129.6 | 8.8 / 46.0 / 131.8 |
| 60 MP, five exports queued | on | 4.47 | 29 GPU, 1 CPU (boundary-pending) | 8.4 / 18.7 / 22.9 | 8.4 / 16.8 / 22.9 | 18.7 / 18.7 / 18.7 | 8.4 / 22.9 / 26.1 |
|  | off | 4.41 | 0 GPU, 30 CPU (preference-off) | 17.5 / 211.5 / 265.4 | — | 17.5 / 211.5 / 265.4 | 26.0 / 217.4 / 274.5 |
| 24 MP at 100%, full Basic | on | 4.97 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 9.4 / 9.6 | 8.3 / 9.3 / 9.4 | 9.6 / 9.6 / 9.6 | 8.3 / 9.3 / 9.4 |
|  | off | 4.67 | 0 GPU, 30 CPU (preference-off) | 9.3 / 11.9 / 12.4 | — | 9.3 / 11.9 / 12.4 | — |
| 24 MP at 200%, full Basic | on | 3.78 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 8.6 / 9.2 | 8.3 / 8.6 / 8.6 | 9.2 / 9.2 / 9.2 | 8.3 / 8.6 / 8.6 |
| X100VI, Texture drag over Presence | on | 4.77 | 29 GPU, 1 CPU (boundary-pending) | 8.5 / 9.5 / 43.5 | 8.5 / 9.3 / 9.5 | 43.5 / 43.5 / 43.5 | 8.5 / 9.5 / 46.4 |
|  | off | 4.45 | 0 GPU, 30 CPU (preference-off) | 41.5 / 47.2 / 48.6 | — | 41.5 / 47.2 / 48.6 | 44.1 / 52.3 / 54.5 |
| X100VI, Clarity drag over Presence | on | 3.64 | 29 GPU, 1 CPU (boundary-pending) | 8.4 / 8.6 / 42.7 | 8.4 / 8.5 / 8.6 | 42.7 / 42.7 / 42.7 | 8.4 / 8.6 / 44.2 |
| X100VI, Basic drag under Presence | on | 3.47 | 29 GPU, 1 CPU (boundary-pending) | 8.5 / 9.0 / 61.7 | 8.5 / 8.8 / 9.0 | 61.7 / 61.7 / 61.7 | 8.5 / 9.0 / 67.7 |
| 24 MP, a 400-position brush stroke | on | 4.34 | 399 GPU, 1 CPU (boundary-pending) | 7.1 / 8.5 / 9.2 | 7.1 / 8.5 / 9.2 | 10.5 / 10.5 / 10.5 | — |

- **Against the 16 ms p95 target.** Met with the GPU preview on at Fit at 24 MP and 60 MP (p95 8.8 to 9.5 ms over two runs each), at 100% (9.4) and 200% (8.6), by each X100VI Presence drag and the Basic drag under Presence (8.6 to 9.5), and by every position of the brush stroke (8.5). Each GPU frame is drawn at the first redraw after its input. With the preview off the same drags' p95 is 18.0 and 25.8 ms at 24 MP, 10.1 and 16.9 ms at 60 MP, 11.9 at 100% and 47.2 on the X100VI's Texture; drawn, the CPU frames reach the screen one to two frames after the GPU's (p50 17.5 to 25.7 ms against 8.3 at Fit).
- **Against the earlier records.** The CPU path's hand-off was 18.1/24.8 ms p50/p95 at 24 MP and 17.2/28.9 at 60 MP on an earlier build; the same build's GPU-off runs now hand off at 9.5 to 17.2 / 18.0 to 25.8 and 9.1 / 10.1 to 16.9, varying by a frame between runs. The GPU frames, measured to their draw, are 8.3/9.3 to 9.5 at 24 MP and 8.3 to 8.4/8.8 to 8.9 at 60 MP, stable across runs.
- **With an exact render holding the pool.** Five `export.jpeg` jobs of the committed stack are queued just before the drag; the lane renders each exactly on the shared pool, then encodes and writes it, one at a time, and an input sent inside the lane's busy window is contended. At 60 MP all 30 inputs of the GPU run were contended: p50/p95/p99 8.4/18.7/22.9 ms, its GPU frames 8.4/16.8/22.9. **The 16 ms target is missed under contention**, by a frame: the interface thread that draws the GPU frame competes with the exports' renders for cores, so some frames wait one redraw more. The 32 ms acceptable limit is met. With the preview off, 15 of its 30 inputs were contended: 18.1/265.4 ms p50/p95 over those, 17.5/211.5/265.4 over the run, the order of the earlier core measurement of a proxy beside exact renders, 207.2/214.7 ms, which was no desktop gesture. At 24 MP the five exports end within about a second, contending the first 8 inputs: the GPU run's p95 is 46.0 ms over all 30 and 39.0 over its 7 contended GPU frames, **missing both 16 and 32 ms**; its uncontended GPU frames also reached 46.0. The 24 MP run with the preview off could not be bounded: its exports each ran under the activity board's 250 ms record and had ended before the read, so the 60 MP run is the contended baseline.
- **The brush stroke.** 400 positions, one every 24 ms, on one brush mask and its masked Basic layer: 399 drawn on the GPU, every position on screen with the first frame that carried it, 7.1/8.5/9.2 ms from its own input, the press 10.6 ms from the first position to its frame, none superseded.

### Memory

The GPU preview stage's high-water over each run (`gpu_preview_peak_bytes`), and the process's lifetime peak footprint as the Performance section last read it, beside the RSS `ps` sampled every 50 ms. The footprint peaks include the open, every exact render and histogram, the evidence captures and, in the contended runs, the exports.

| Case | GPU preview | GPU-preview high-water | Process peak footprint | Sampled peak RSS |
| --- | --- | ---: | ---: | ---: |
| 24 MP JPEG at Fit | on | 32.5 MB | 1569 MiB | 1176 MiB |
| 24 MP JPEG at Fit | off | 0.0 MB | 1409 MiB | 1203 MiB |
| 60 MP JPEG at Fit | on | 30.9 MB | 1727 MiB | 1573 MiB |
| 60 MP JPEG at Fit | off | 0.0 MB | 2039 MiB | 1601 MiB |
| 24 MP at 100% | on | 40.7 MB | 1811 MiB | 1287 MiB |
| 24 MP at 100% | off | 0.0 MB | 1630 MiB | 1328 MiB |
| 24 MP at 200% | on | 10.2 MB | 1727 MiB | 1255 MiB |
| 60 MP, five exports queued | on | 30.9 MB | 2148 MiB | 1697 MiB |
| 60 MP, five exports queued | off | 0.0 MB | 2384 MiB | 1987 MiB |
| X100VI, Texture over Presence | on | 95.9 MB | 2595 MiB | 2034 MiB |
| X100VI, Texture over Presence | off | 0.0 MB | 2515 MiB | 2085 MiB |
| X100VI, Basic under Presence | on | 95.9 MB | 2658 MiB | 2080 MiB |
| 24 MP brush stroke | on | 32.5 MB | 1053 MiB | 665 MiB |

- **The GPU preview's own share** is 10 to 41 MB at Fit, 100% and 200% on the JPEGs and 96 MB over the X100VI's `f32` boundary with Presence, within the 2 GiB budget, which bounds the heaviest 100% slots the corpus measures (Detail then all three Presence fields, 1,123.8 MB with Texture's band in one channel; [the corpus](#the-corpus)) and the slots of a mask painted over masked Presence layers (0.26 to 0.82 GB at Fit over 3 to 16 masks and 0.54 GB at 100% over three, [painting](#painting-over-masked-spatial-layers)). A boundary arriving at 100% holds up to 2.2 times its bytes during its upload, and the system allocator keeps the freed copy's pages in the footprint afterwards ([a boundary's arrival](#a-boundarys-arrival)).
- **Against the provisional 1 GiB target for one 60 MP photograph**, which `measure` takes over one open: these gesture runs exceed it, with the GPU preview on and off alike — 1.69 and 1.99 GiB peak footprint for the 60 MP drags, 1.54 and 1.56 GiB sampled RSS — so the GPU preview is not what takes them past it. The peaks vary between runs by more than the GPU preview's share.

### Idle after a dissolve

The 24 MP drag with `--idle`: after its release had dissolved from the drag's last GPU frame (one dissolve started and ended, none cancelled), with the Performance section closed, a 4-second settle and a 10-second window drew one frame and built one view, the window's own, and the process spent 15.1 ms of CPU, 0.15% of one core. The same catalog reopened in an ordinary launch, the Performance section open, used 0.83% of one core over 30 seconds, under the 1% idle target.

### Windows and Linux

That qualification ran only on the M4. Later container software checks and actual hosted results are recorded in [development](../engineering/development.md#ci): Linux no-adapter checks, release acceptance and packaging pass; the retained lavapipe journeys use small GPU-stage fixtures and run to aggregate failure, with a passing hosted result still open. Native Windows/Linux GPU acceptance remains unrun; a configured lane is not a passing result. Windows CI is disabled.

### The performance-rules checklist

For GPU previews as a whole ([rules](../engineering/performance-rules.md#review-checklist)):

- **The original.** Read, hashed and decoded only as before: every boundary is derived on the GPU from the photograph's source the surface holds, uploaded once from the verified prepared source.
- **Full-frame allocations.** On that build the held boundary (the worker's texels over only the window of its stage the drawn output reads, an `Arc` the desktop let go once the surface held them, at most 256 MiB each), the surface slot's boundary texture, output, spatial planes and a chain's link intermediates within the 2 GiB GPU-preview budget, which refuses a slot past it and names the CPU path, a warp's grid of at most 2 MiB, and 32 MiB of upload staging a frame. Nothing is cloned that is shared.
- **Point queries, validation and no-op checks** render no frame: a plan is built from the compiled stack's descriptions in O(layers), and on that build samples, analysis and export stayed on the CPU, byte for byte.
- **The owner thread** plans a tick's GPU plan in the draft's own answer and checks a boundary's size against its bound; all are arithmetic over descriptions, no frame work. On that build it also planned, at the exact stage at Fit, the window its output read through the CPU's windowed planner in `O(segments)`, since deleted. For a Basic drag over a 480 × 320 photograph drawn at its exact stage the plan's p50 was 3.8 to 4.9 µs whole and 5.9 to 7.7 µs under a straightened crop, of which the window was 0.04 to 0.12 µs (the ignored `exact_fit_window_planning`, deleted with the planner, the `test` profile on the M4, three runs at one-minute loads of 25 to 60, 2026-10-03; a measurement, not a baseline).
- **Desktop messages.** A tick drawn on the GPU sends no preview job and uploads no frame; on that build a gesture's first tick's job carried its one boundary request. `asset.state` and `history.list` are unchanged.
- **Timers, polls and subscriptions.** None added. The compile thread blocks on its queue, a pass's completion is reported by a later submit with no poll, and the settle dissolve asks for redraws only while it runs, at most 150 ms; idle after a dissolve is 0.15% of one core.
- **Repeated work.** On that build the boundary was rendered once per draft and held (key: the source, the layers before it, its layer and the proxy plan, or at the exact stage at Fit the window its output reads), and compiled program sequences were cached, eight per pipeline, and warmed when the stack changed; a spatial pass runs only when what it reads changed (5 compute passes for a Texture tick at 100%, none for Clarity).
- **editor-performance.** GPU previews change no core render; the `timing` tier's `editor-performance` run is the before-and-after for the CPU path, and the same build's GPU-on and GPU-off drags above are the gesture's.
- **Exactness.** Each program is qualified against its CPU unit on dense synthetic grids and on the corpus, within its class's limits, and the CPU's frames, samples, analysis and API answers keep their exact bytes, which the core's exact-buffer tests hold.

### Verification

`verify --tier quick` and `--tier rendered` (with the private RAW manifest, 48 components) passed on the release editor above. `verify --tier timing` ran every component, but each timing component started above the 8.0 load threshold (10.0 to 10.6, raised by the tier's own `check` just before them), so it reports every timing verdict as unreliable rather than as a pass or a miss. `measure` was therefore run again on its own at a load of 4.3, and then, to attribute what it missed, over the editor built from `46a85159` (the base before GPU previews) and the current editor back to back, at loads of 3.0 to 4.3, with the same harness:

| `measure` target | Before GPU previews | Now | Now, alone | Verdict |
| --- | ---: | ---: | ---: | --- |
| Launch to usable empty shell, p95 < 1 s warm | 1378 / 1476 ms | 1436 / 1472 ms | 1430 / 1489 ms | **Missed**, before GPU previews too |
| Uncached 24 MP JPEG to Fit preview, p95 < 750 ms | 779 / 826 ms | 783 / 827 ms | 784 / 824 ms | **Missed**, before GPU previews too |
| 24 MP working set ≤ 600 MiB, sampled RSS | 279 / 313 MiB | 276 / 312 MiB | 284 / 317 MiB | Met |
| 60 MP peak ≤ 1 GiB, sampled RSS, one open | 429 / 434 MiB | 419 / 452 MiB | 418 / 453 MiB | Met |
| Idle CPU < 1% of one core over 30 s | 1.5% | 1.6% | 0.49% | **Missed** in the back-to-back pass, before GPU previews too; met alone |

p50 / p95 over five launches (ten opens of the 24 MP JPEG). The launch figure is an upper bound: it runs from the spawn of a fresh background bundle with its copied 35 MB executable to the first captured frame, of which the editor's own startup to that frame is 0.2 s; the open figure runs from the request to the decoded raster. Neither changed with GPU previews beyond the run-to-run spread. The idle window includes the open Performance section's sampler and moved between 0.5% and 1.6% of one core across runs of either build.

## GPU qualification against the reference

The release gate ([GPU-first](../design/gpu-first.md#the-contract)): `cargo xtask gpu-qualification` over the whole [corpus](../../fixtures/preview/corpus.json), every stack rendered on the reference renderer and on the GPU at Fit, 33%, 50% and 100%, and every output kind the GPU renders held to its limit: the picture at rest (1,044 cells), the histogram and clipping counts, samples and export (261 stacks each), with no gap and no error. The last full run passed on 2026-10-06, as the release gate inside `verify --tier rendered`. The GPU's frames are the editor's own, drawn by the photo surface's own drawing on a headless device from the photograph's source held on the GPU as the editor holds it ([GPU previews](../design/gpu-preview.md#the-gpu-source)): the picture at rest, and the frame a drag draws. A measurement of pixels, not of time. By the [recorded default](../design/gpu-first.md#proposals-with-recorded-defaults), which gates the picture at rest against the reference and reports the picture in motion, the gate passed: the picture at rest within its limits on every cell, and the histogram, samples and export within theirs on every stack. The picture in motion is within its limits at 100% and past them on 462 of the 783 cells at Fit, 33% and 50%, against the picture at rest it settles to and against the reference alike. The limits stand under every reading. Dehaze's light is the per-frame light, computed by its light link from the whole stage at full resolution at every view ([GPU previews](../design/gpu-preview.md#spatial-programs)), the stand-in with the spatial layers before it left out for a light behind Detail or another Presence layer.

### Scope

- **What was compared.** At each view two GPU frames. The picture at rest: at Fit, 33% and 50% the editor's picture at rest in tiles, the whole output stage drawn at full resolution in tiles of 2048 px or smaller, each cut from the source, reduced to the view's size by the area-weighted average the reference is reduced by and quantized (1 to 240 tiles a stack, the most for Detail beside Presence, masked, on the 60 MP JPEG at Fit); and where the view draws the output at its own size or larger, as at Fit for the Presence fixture and the tight crop (40 cells), and at 100%, the view plan over the visible window, cut from the source at full scale. The frame a drag draws: the plan from the source over the boundary the surface derives, the source reduced to the view's proxy at Fit and below 100% and cut at 100%. Fit is the display-bounded proxy of an evidence run's 1440 × 900 window at 2× (1716 × 1508 bounds), 33% and 50% the displayed size of the whole output stage within the display bounds (4096 px a side, 8 MP), and 100% the visible region of the owner's largest window (1512 × 982 logical at 2×, panels closed) at full scale. The reference is the reference renderer's exact whole frame, as export renders it, reduced to the GPU frame's size by an area-weighted average of its linear light (`luxforge_reference::tolerance`), or at 100% its visible region, unreduced. Each comparison is the four statistics of the [preview error measure](../design/gpu-preview.md#the-preview-error-limit), each recipe held to its class's limits (pointwise 0.5 / 1.0 / 2.0 / ±0.25, spatial 1.0 / 2.5 / 5.0 / ±0.5).
- **Host and build.** The `Apple M4 Pro` adapter on Metal; the harness built in release from `55c42f38` (harness SHA-256 `477cf21d…`, clean working tree), 2026-10-06, the whole corpus in one run of `verify --tier rendered`, whose report is `target/integration-logs/verify-rendered-2/gpu-qualification/` in the worktree that built it. On one machine and driver the pixels are deterministic, so the host's load does not bear on them.
- **Corpus.** `fixtures/preview/corpus.json` at SHA-256 `03e716c7…`: 42 recipes, 261 stacks (106 pointwise, 155 spatial), each at four views, over the generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture, and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest, all eight verified and their SHA-256s unchanged after the run.
- **Memory.** The run records no heaviest-slot figure. The last measured one is from the 2026-10-05 run (build `21fc9c48`, 250 stacks): every tile and every drag's slot fit the 2 GiB budget beside the source, the heaviest, a drag's slot at 100% of Detail then all three Presence fields on the Air 2S over its anchored 3970 × 2782 window, charging 1,466.4 MB with its light link, beside the Air 2S's 240 MB source.
- **Output kinds.** The picture at rest (1,044 cells, 0 past a limit, 0 gaps, 0 errors), the picture in motion (reported, not gated), the histogram and clipping counts (261 of 261 within), samples (261 of 261 within, 6,525 points all equal to the byte on screen) and export (261 of 261 within the display limit and the same bytes twice, no gap). The GPU renders every kind, so none is reported as not rendered.

### The picture at rest against the reference

What the gate judges; mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / signed mean ΔL\*, the largest magnitude over a view's cells of a class and their mean.

| View | Class | Measured | Within the limits | Past them | Largest: mean / worst block / p99 / \|ΔL\*\| | Mean over the cells |
| --- | --- | ---: | ---: | ---: | --- | --- |
| Fit | pointwise | 106 | 106 | 0 | 0.042 / 0.251 / 0.851 / 0.009 | 0.005 / 0.038 / 0.190 / 0.000 |
| Fit | spatial | 155 | 155 | 0 | 0.058 / 0.509 / 0.955 / 0.049 | 0.008 / 0.062 / 0.241 / 0.000 |
| 33% | pointwise | 106 | 106 | 0 | 0.041 / 0.135 / 0.857 / 0.010 | 0.005 / 0.033 / 0.190 / 0.000 |
| 33% | spatial | 155 | 155 | 0 | 0.057 / 0.397 / 0.960 / 0.049 | 0.008 / 0.060 / 0.236 / 0.001 |
| 50% | pointwise | 106 | 106 | 0 | 0.080 / 0.234 / 0.862 / 0.008 | 0.006 / 0.045 / 0.191 / -0.000 |
| 50% | spatial | 155 | 155 | 0 | 0.087 / 0.397 / 0.963 / 0.049 | 0.009 / 0.079 / 0.253 / 0.001 |
| 100% | pointwise | 106 | 106 | 0 | 0.046 / 0.251 / 0.851 / 0.009 | 0.005 / 0.049 / 0.184 / 0.000 |
| 100% | spatial | 155 | 155 | 0 | 0.058 / 0.509 / 0.954 / 0.049 | 0.009 / 0.084 / 0.262 / 0.001 |

- **Every cell is within its limits**, with no gap and no error, the largest a mean of 0.087, a worst block of 0.509 and a p99 of 0.963: the stack processed at full resolution and reduced after, as the reference is, leaves only the GPU's arithmetic and storage between the two, and Dehaze's light the whole stage's. The largest p99 of the spatial cells at every view is Detail beside Presence on the Z6 (0.955 to 0.963), whose light behind Detail was in that build the stand-in with Detail left out ([the light behind a spatial layer](#the-light-behind-a-spatial-layer) for the exact light).
- **What drew it.** At Fit, 33% and 50% the picture at rest in tiles (at Fit, the view plan too, where the view draws the output at its own size or larger); at 100% the view plan.

### The picture in motion

Reported, not gated; the frame a drag draws at each view against the picture at rest it settles to and against the reference, the largest magnitude over a view's cells of a class.

| View | Class | Measured | Against the picture at rest: within / past | Largest | Against the reference: within / past | Largest |
| --- | --- | ---: | --- | --- | --- | --- |
| Fit | pointwise | 106 | 46 / 60 | 11.3 / 28.2 / 38.0 / 9.11 | 46 / 60 | 11.3 / 28.1 / 38.0 / 9.11 |
| Fit | spatial | 155 | 70 / 85 | 32.2 / 51.9 / 60.9 / 40.1 | 70 / 85 | 32.2 / 51.9 / 60.9 / 40.1 |
| 33% | pointwise | 106 | 29 / 77 | 12.0 / 29.7 / 52.1 / 8.73 | 29 / 77 | 12.0 / 29.7 / 52.1 / 8.73 |
| 33% | spatial | 155 | 67 / 88 | 29.8 / 52.9 / 61.2 / 37.3 | 67 / 88 | 29.8 / 52.9 / 61.2 / 37.3 |
| 50% | pointwise | 106 | 34 / 72 | 11.7 / 23.1 / 39.0 / 5.69 | 34 / 72 | 11.7 / 23.1 / 39.0 / 5.69 |
| 50% | spatial | 155 | 75 / 80 | 19.1 / 44.8 / 64.4 / 18.1 | 75 / 80 | 19.1 / 44.8 / 64.4 / 18.1 |
| 100% | pointwise | 106 | 106 / 0 | 0.000 / 0.000 / 0.000 / 0.000 | 106 / 0 | 0.046 / 0.251 / 0.851 / 0.009 |
| 100% | spatial | 155 | 155 / 0 | 0.000 / 0.000 / 0.000 / 0.000 | 155 / 0 | 0.058 / 0.509 / 0.954 / 0.049 |

- **At 100%** every cell is within its limits, and the drag's frame is the picture at rest's to the bit, both the region plan over the same window cut from the source.
- **At Fit, 33% and 50%** 462 of the 783 cells are past a limit, the same cells against the picture at rest and against the reference: 209 of 318 pointwise cells and 253 of 465 spatial ones. The drag's frame processes a source reduced to the view's size, where the picture at rest and the reference reduce the processed frame, so detail beyond the view's resolution, and every neighbourhood and estimate a spatial unit takes, meets the stack's arithmetic after the reduction in one and before it in the other. Every source has cells past the limits (the Air 2S 105, the zone plate 92, the Z6 75, the 24 MP JPEG 62, the 60 MP JPEG 55, the X100VI 31, the Presence fixture 24 and the lens zone plate 18); the Presence fixture none at Fit, where it is drawn at its own size and the reference is not reduced. The largest figures, up to 32.2 / 52.9 / 64.4 / 40.1, are all on the zone plates and the lens zone plate: under crops, warps and the luminance range, and for the spatial units under Dehaze, whose light is the whole stage's at full resolution while its transmission is taken over the reduced stage. Gated by either `--gate-motion` reading, these 462 cells fail it.

### The softer frame at 100%

The frame a drag draws at 100% where the region's slot would pass the GPU-preview budget beside the source (`budget-reduced`, "Softer while dragging"), which the owner accepted on 2026-10-07 subject to explicit qualification. `gpu-qualification` measures it on every 100% cell and reports it, never gated: the stack planned whole at the reduced stage of the view's area, `GpuView::Fit` at the region's displayed size as the desktop plans it, drawn on the GPU and magnified to the region bilinearly in linear light, standing in for the photo surface's sampler, which the headless surface does not run. Each cell records whether a drag there would draw it by the budget (`natural`) or it was drawn to qualify it (`forced`). Run on 2026-10-08 at 100% alone, built from `3ba5b282` with the softer frame added, the `Apple M4 Pro` adapter, the corpus and the owner's RAW manifest; `artifacts/softer-qualification-1/` in that worktree.

| Class | Measured | Natural | Against the picture at rest: within / past | Largest | Against the reference: within / past | Largest | The reduced frame against the reference reduced: within / past | Largest |
| --- | ---: | ---: | --- | --- | --- | --- | --- | --- |
| pointwise | 106 | 0 | 21 / 85 | 14.0 / 26.5 / 61.1 / 13.2 | 21 / 85 | 14.0 / 26.5 / 61.1 / 13.2 | 45 / 61 | 8.45 / 16.4 / 31.7 / 7.51 |
| spatial | 171 | 0 | 26 / 145 | 16.7 / 38.5 / 62.5 / 12.7 | 26 / 145 | 16.7 / 38.6 / 62.5 / 12.7 | 80 / 91 | 21.9 / 46.2 / 68.2 / −28.1 |

- **No corpus cell draws it by the budget**: every region's slot fits the 2 GiB budget beside its source, as the design says. It is reached past the corpus: Detail beside Presence's three fields in a window about 1.4 times the owner's display's, or more than five masked Presence layers in the centred view ([at 100% and above](../design/gpu-preview.md#at-100-and-above)).
- **It is soft, as accepted.** Against the reference's region, the mean ΔE00 is 0.67 at the median pointwise cell and 0.58 at the median spatial cell, and about 12 at the 90th percentile of both, the largest on the generated detail and texture sources, where magnifying a frame of the view's size loses the fine detail the region holds. 230 of the 277 cells are past the motion limits. It is as far from the picture at rest it settles to as from the reference, because the picture at rest is within the at-rest limits on every one of these cells.
- **The reduced frame itself** is as far from the reference reduced to its size as the Fit motion frame above is: it processes a source reduced to the view's size, the same proxy.
- The at-rest gate at 100% in the same run: every cell within its limits.

### The GPU export against the reference export

`gpu-qualification --kind export` on 2026-10-05, built from `4e793fe3`, the same host, corpus and manifest, report under `artifacts/gpu-first/task-005/export/` in that worktree. Each of the 250 stacks was exported through the desktop's GPU tile worker on the `Apple M4 Pro` adapter and its codes compared over every pixel with the reference export's by the display limit of its class, then exported again through a second worker's own device.

| Class | Stacks | Within the limit | Same bytes twice | Largest: mean / worst block / p99 / \|ΔL\*\| |
| --- | ---: | ---: | ---: | --- |
| pointwise | 106 | 106 | 106 | 0.110 / 0.308 / 0.851 / 0.009 |
| spatial | 144 | 144 | 144 | 0.058 / 0.509 / 0.962 / 0.049 |

- **No gap.** Every stack was drawn on the GPU, the 74 that read Dehaze's light among them: the tile worker's runner computes each light once for the export, from the whole stage at full resolution a window of the source at a time, before the tiles that read it; a light behind Detail was in that build the stand-in with Detail left out, as on screen then. At the merge before it (`21fc9c48`) those 74 were gaps, the runner computing no light.
- A selection of one kind, so the run reports itself incomplete; its export rows are the whole corpus.
- **The full gate of 2026-10-06** (build `55c42f38`, 261 stacks) holds the export within its limit on all 261: no gap, the same bytes twice, the largest 0.119 / 0.509 / 0.962 / 0.049 ([the full gate](#gpu-qualification-against-the-reference)).

### The histogram and clipping counts

`--kind histogram` on 2026-10-05 over all 250 stacks of the corpus, the M4 Pro's Metal adapter, with the RAW manifest: a measurement of counts, not of time (`artifacts/gpu-first/task-004/gate-4`). The GPU's counts are the editor's own: each stack's tiles at full resolution as a committed job plans them (1 to 240 tiles of 2048 px or smaller), each over its window cut from the source, Dehaze's light computed on the GPU per frame, drawn by the photo surface's own drawing for their counts alone and counted by its histogram reduction; the same tiles are read back whole for the luminance histogram and the diagnostics, and their codes count to the GPU's counts exactly on every stack. The reference is the core's reducer over the reference renderer's exact whole frame. The limit, the owner's of 2026-10-05: the earth mover's distance between the histograms within 0.25 code on each of R, G, B and luminance, and each clipping counter within 0.1% of the output pixel count.

- **Within the limit on all 250 stacks.** The largest earth mover's distance is 0.130 codes, on R, G, B and luminance alike (Basic under Dehaze with a crop, Presence fixture: a systematic shift of +0.12 codes, 15.5% of its pixels one code brighter); 249 stacks are within 0.05 and 240 within 0.02. The largest clipping difference is 0.0313% of the output pixels (`r255`, Texture with Clarity on the Presence fixture). 4 stacks are exact.
- **Reported beside it, not gated.** The summed absolute bin difference is past 0.1% of the pixels on 89 stacks (34 of 106 pointwise, 55 of 144 spatial; 29 of 35 on the Presence fixture, 22 on the zone plate, 15 and 14 on the 60 and 24 MP JPEGs, 3 of 5 on the lens zone plate, 6 of 105 RAW stacks), the largest 23.6% on the stack above, 7.4% for all three Presence fields there; binned at 2 codes 69 of them are still past 0.1%, and at 4 codes 50, since a flat patch one code across a bin's edge moves whole. Pixel by pixel, the frame differs from the reference frame by more than two codes on at most 0.455% of a stack's pixels (the perspective and lens warp over the lens zone plate's chirp, whose resampling also moves 31% of its pixels by one code) and on none of most stacks; the all-three-Presence-fields stacks on the Presence fixture reach 0.29%. Eight stacks moved further when the light became per frame, the five with Detail before Presence by a systematic shift of up to 0.042 codes, which the light computed with Detail left out ([GPU previews](../design/gpu-preview.md)) would give.
- **The full gate of 2026-10-06** (build `55c42f38`, 261 stacks) holds the histogram within its limit on all 261: the largest earth mover's distance 0.1296 codes (`r`, Basic under Dehaze with a crop, Presence fixture), the largest clipping difference 0.0313% of the output pixels, exact on 4 ([the full gate](#gpu-qualification-against-the-reference)).
- **What it means.** The GPU's reduction counts its codes exactly ([basic and histogram](../design/basic-and-histogram.md#histogram-and-clipping-contract)). A summed difference counts a pixel whose code moved one step twice, so a share of a flat patch at a code's rounding edge moving by one code passes 0.1% of the pixels while the histogram moves by a hundredth of a code; the earth mover's distance measures how far it moved.

### Samples from GPU tiles

`gpu-qualification --kind sample` on 2026-10-05, built from `df33bc27`, the same host, corpus and manifest, report under `artifacts/gpu-first/task-006/gate/` in that worktree. Each of the 250 stacks is read at the 25 points of a 5 × 5 grid over its output stage by the desktop's GPU tile worker, as `render.sample` reads a pixel through it — one call, its points one session — against the reference frame's bytes there by the display limit of the stack's class (mean, p99 and signed mean ΔL\* over the 25 samples), and against the byte the picture at rest shows at each point at 100%, drawn by the photo surface over a 256 px window around it, which it must equal.

| Class | Stacks | Within the limit | Samples | Equal to the screen | Largest: mean / p99 / \|ΔL\*\| / single |
| --- | ---: | ---: | ---: | ---: | --- |
| pointwise | 106 | 106 | 2,650 | 2,650 | 0.104 / 0.924 / 0.018 / 0.924 |
| spatial | 144 | 144 | 3,600 | 3,600 | 0.078 / 1.006 / 0.048 / 1.006 |

- **No gap.** Every point was answered by the GPU, none by the reference; 39 stacks hold a sample that differs from the reference's byte, by at most 1.006 ΔE00.
- **The byte on screen.** All 6,250 samples equal the picture at rest's byte at their pixel at 100%, which the anchored windows make so by construction.
- **Latency.** Not measured here; through three spatial segments on the Air 2S a GPU sample answers in 85.4 / 91.7 ms p50 / p95 warm ([a three-segment sample](#a-three-segment-sample)).
- A selection of one kind, so the run reports itself incomplete; its sample rows are the whole corpus.
- **The full gate of 2026-10-06** (build `55c42f38`, 261 stacks) holds samples within the limit on all 261, every one of the 6,525 points equal to the byte on screen: the largest mean 0.104, p99 1.006, \|signed mean ΔL\*\| 0.048 and single 1.006 ([the full gate](#gpu-qualification-against-the-reference)).

### Colour after Detail, before a resample

The corpus's three Detail recipes with colour between Detail and the geometry — `detail-curve-lens-perspective` on the three RAWs with their own lens profiles, `detail-curve-lens-perspective-jpeg` on the zone plate with a lens identity, and `detail-curve-straightened` on all seven sources — which the GPU draws since the colour runs on Detail's output before the tail ([GPU previews](../design/gpu-preview.md#where-the-code-lives)). `gpu-qualification --recipes` on 2026-10-06, the same host, built from `f9ad9f30` with the change uncommitted, every kind: the 11 stacks at four views, 44 cells of the picture at rest, every one within the spatial limits.

| View | Cells | Within the limits | Largest: mean / worst block / p99 / \|ΔL\*\| |
| --- | ---: | ---: | --- |
| Fit | 11 | 11 | 0.035 / 0.116 / 0.693 / 0.006 |
| 33% | 11 | 11 | 0.044 / 0.108 / 0.692 / 0.006 |
| 50% | 11 | 11 | 0.087 / 0.213 / 0.695 / 0.006 |
| 100% | 11 | 11 | 0.049 / 0.203 / 0.696 / 0.006 |

- **The other kinds.** The 11 exports, histograms and sample grids are within their limits: the largest export 0.119 / 0.353 / 0.774 / 0.006, the largest histogram distance 0.0188 codes, every sample equal to the byte on screen.
- **In motion, not gated.** A drag's frame at 100% is the picture at rest's; at Fit, 33% and 50%, over the source reduced to the view, 21 of 33 cells are past the limits against the picture at rest, as the corpus's other stacks are ([the picture in motion](#the-picture-in-motion)).
- **The lens and crop families**, run in the same build: all 116 cells of the picture at rest within the pointwise limits, the largest 0.080 / 0.234 / 0.862 / 0.001 at 50%.
- A selection, so each run reports itself incomplete.

### Reproducing it

```sh
cargo xtask preview-corpus --manifest /path/to/raw-manifest.json
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --gate-motion against-rest
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind histogram
```

Each run writes `report.json` (every cell's figures and verdict, the reading, the limits, the build and corpus identity and the sources' hashes before and after), `summary.md`, `harness.log`, the harness's own `run/cells.json` and the frames of every judged cell past a limit under `run/frames/` (`--frames all` for every cell). Without `--manifest` the RAW stacks are gaps, so a run within every limit would still be incomplete; `--zoom`, `--kind`, `--families`, `--recipes` and `--sources` measure a selection, which is incomplete too.

## GPU-first against the 2026-10-04 baseline

The latencies of an ordinary edit with the GPU as the renderer of record, measured against the [baseline](../design/gpu-first.md#the-baseline-this-replaces) the GPU-first plan replaced (TASK-011 in [the plan](../../tasks/rendering/gpu-first.json)). The release gate is not run again here: it passed on the whole corpus at `55c42f38` inside `verify --tier rendered` ([above](#gpu-qualification-against-the-reference)), and the source measured below adds only documents to that build.

### Scope

- **Host.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal, a hidden 1440 × 900 window at 2× (2880 × 1800 physical) in a background-only bundle, warm filesystem cache, 6 October 2026, 15:17 to 16:11 BST, 54 minutes of measuring. No other session built or tested on the host: no `rustc`, `cargo`, `clang` or `ld` process was running at the start of any run. The owner's own applications were open; Chrome took up to one core during the export runs. The one-minute load at the start of each run is given with its figure; every run quoted started below the 8.0 threshold and all but two below 5 (the first 24 and 60 MP export probes at 5.1 and 6.4, repeated at 3.4 and 2.6 and quoted from the repeats; Dehaze's 60 MP drag at 100% at 4.6). A reference-renderer run raises the load itself, so each was started only once the load had fallen below 3.5.
- **Build.** Source `54d910a4` (`claude/gpu-first-9b5b17`), release, `--locked`, Cargo.lock SHA-256 `0ee75f59…`. Two executables of that source: `d0867b9d…` from `cargo build --release --locked -p luxforge-app -p xtask`, which the drag, commit and paint runs on the GPU, the reference renderer's runs quoted and the timing tier used, and `3f08f163…` from the whole workspace's build that `cargo xtask develop --background` makes, whose feature unification differs, which the evidence-script probes (samples, export, Compare and history, the cold shader cache, idle memory) and the white-balance and Dehaze runs used. Each report records its own `binary_sha256`.
- **Sources.** The Air 2S DNG of the private manifest (5464 × 3640, 19.9 MP, SHA-256 `aab79ce1…`; the design's "16 MP" is this file), the generated 24 MP and 60 MP JPEGs (`b54c2a15…`, `b9e0118a…`). The three RAWs of the manifest hashed the same before and after every run.
- **Stacks.** *The drag stack*: Detail (sharpening 60, luminance 40, colour 40), Presence's three fields at +100 and a full Basic layer, every field non-neutral (`editor-latency --detail --presence --basic`), the measured gesture a Basic exposure drag, which runs before Presence, so every tick runs the whole chain and computes Dehaze's light again. *The masked stack*, the design's freeze stack: Detail, a global Presence of Texture 25 and Clarity 20, and three masks, a brush and two radials, each holding a masked exposure and a masked Presence of Clarity 50 and Texture 40 (`editor-latency --mode paint --masks 3 --mask-presence --presence --detail`). `editor-latency` cannot draft a masked slider over a stack that holds no masked layer yet: the drag creates the layer, its first or second tick compiles the new link (40 ms drawn, or held 100 ms as `compiling`, in two attempts with `--mask`), and the harness refuses a held tick as an input with no answer, so the masked stack is measured as a painted stroke and the drag stack as the heaviest slider drag the tool builds. *The three-segment stack* of the samples: Detail as above, Presence's three fields at +100 and a radial's masked Presence of Clarity 50 and Texture 40, the Air 2S's lens profile after them.
- **Figures.** Milliseconds, nearest rank. A GPU frame is timed to the surface's first draw of its plan, a CPU frame to its hand-off ([development](../engineering/development.md)); neither is scanout, and on the 120 Hz display a frame is 8.3 ms. Where fewer than 30 samples were taken the median and range are given and no p95.

### Ticks and commits on the Air 2S

The drag stack, 30 drained inputs per drag and 30 commits per commit run (`--mode commit`: one input and its release each), every tick drawn on the GPU; release to the committed frame is the GPU's view plan presented in place, release to the picture at rest is its first draw once its last tile is in (`gpu_rest_drawn`, at Fit and 33%; at 100% the view plan is the picture at rest), and the counts are the GPU's settled histogram (`analysis_adopted`).

| View | Load | Drag tick p50 / p95 | Commit: committed frame p50 / p95 | Commit: picture at rest p50 / p95 | Commit: settled histogram p50 / p95 (from the release) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Fit | 2.97, 3.48 | 9.0 / 9.2 | 9.8 / 17.1 | 770.8 / 780.6 | 816.3 / 825.8 |
| 33% | 3.65, 2.31 | 9.0 / 9.6 | 9.9 / 17.2 | 765.8 / 779.7 | 810.2 / 823.3 |
| 100% | 2.99, 2.58 | 9.0 / 9.3 | 9.8 / 10.0 | (the view plan) | 838.3 / 846.3 |

- **Every tick within a display frame.** 30 of 30 inputs at each view on the GPU, none held; the 16 ms target is met at every view. The baseline drew 33% on the CPU proxy, roughly 80 to 300 ms a tick at 60 MP, and a Basic drag under Dehaze at 100% computed an exact-stage estimate per tick, 110 to 420 ms.
- **The commit's frame** is the GPU's view plan of the committed stack at once, 10 ms; its picture at rest in tiles, 24 tiles of 1024 px with a 300 px lead, dissolves in about 0.77 s later and its counts 45 ms after that. The reference renderer's whole frame and report for the same commits took 792.8 / 878.6 ms (`--reference-renderer`, load 3.25), so on this stack the GPU's sharp picture and counts arrive about as soon as the reference's would, while the edit itself is on screen at once.
- **The masked stack, painted.** A 240-position stroke over the three masks: at Fit every position on the GPU at 8.0 / 8.7 ms (load 2.30); at 100%, 237 positions on the GPU at 8.2 / 23.7 ms, worst 50.4 (load 2.88), **missing the 16 ms target at p95**. A second run at Fit drew 8.1 / 9.0 ms (load 3.39). The reference renderer's session draws the same stroke at Fit at 102.6 / 115.3 ms over the 65 positions it presented. Over the 24 MP JPEG's paint harness at 100% a stroke over three masks stayed within a frame ([painting](#a-strokes-latency)); on the Air 2S with Detail and the global Presence beside them it does not.
- **The masked stack's picture at rest misses badly.** After the stroke's release the GPU presents the view plan at once, but its picture at rest is planned in 256 px tiles with a 720 px lead, 330 tiles, and was drawn 17.05 and 16.99 s after the release in two runs, its counts 50 ms later (loads 2.30 and 3.39); the reference renderer's whole frame and report took 1.26 s for the same release (load 3.38). One masked Presence layer is enough: committing a radial's masked Presence of Clarity 50 and Texture 40 over Detail and Presence's three fields (the three-segment stack) drew its picture at rest, 330 tiles of 256 px with a 480 px lead, 14.9, 15.7, 15.9 and 17.3 s after the commit in four launches, the counts 60 ms later: about 45 to 50 ms a tile, one tile a frame, each tile's window starting its lead (`GpuAnchor`, the running sums' reach) and its halos before the 256 px it draws. Until then the histogram is the view plan's, labelled updating.

### The settled histogram after a commit

The same drag stack, `--mode commit`, from the release to the GPU's counts adopted, and the same executable's reference renderer for the same commits (`--reference-renderer`):

| Source | Samples | Load | GPU: picture at rest p50 / p95 | GPU: settled histogram p50 / p95 | Reference: frame and histogram p50 / p95 | Tiles |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 24 MP JPEG | 30 | 2.42 (reference 3.39) | 603.1 / 607.4 | 649.4 / 653.4 | 671.9 / 775.8 | 24 of 1024 px |
| Air 2S DNG | 30 | 3.48 (reference 3.25) | 770.8 / 780.6 | 816.3 / 825.8 | 793.1 / 878.9 | 24 of 1024 px |
| 60 MP JPEG | 6, and 8 of an earlier run | 2.92 (reference 2.67) | 5,620.9, 5,613 to 5,678 | 5,676.9, 5,646 to 5,732 | 1,682.3, 1,659 to 1,919 | 240 of 512 px |

- **At 60 MP the GPU's picture at rest and counts miss**: 5.6 to 5.7 s against the reference renderer's 1.7 s for the same stack on the same host, and against the baseline's "about 3 s at 60 MP" for the exact render after every commit. The rest is planned in 240 tiles of 512 px with a 300 px lead, one tile a frame at about 23 ms each. Only 6 commits fit the tool's 60 s deadline (an earlier 30-commit run stopped after 9, whose 8 completed commits agree), so no p95 is claimed. At 24 MP the GPU's counts come 22 ms sooner than the reference's at p50 and 122 ms at p95; on the Air 2S 23 ms later at p50 and 53 ms sooner at p95: the same within the spread.

### A three-segment sample

`render.sample` through the three-segment stack on the Air 2S, through the desktop's GPU tile worker, each call an `api` step of an evidence script launched with `cargo xtask develop --background --hidden-window --evidence-dir` and timed from the step to its answer (`script_step` to `script_step_settled`, `host_answered`), 31 points at random over the stage, after the picture at rest and the warm-up had finished. Every answer named `renderer` `{record: "gpu"}`.

| Run | Load | Samples | p50 / p95 | Range |
| --- | ---: | ---: | ---: | ---: |
| Warm: the shared bundle, after the launch's first sample | 2.35 | 30 | 85.4 / 91.7 | 70.9 to 93.9 |
| The launch's first sample, the shared bundle | 2.35 | 1 | 140.9 | |
| The launch's first sample on a fresh bundle's empty Metal cache (`LUXFORGE_BACKGROUND_BUNDLE_SUFFIX`) | 3.81 to 4.36 | 3 launches | 158.3 | 156.5 to 164.1 |
| The two after it on those fresh bundles | 3.81 to 4.36 | 6 | | 89.6 to 97.6 |

- **The target is met**: warm p95 91.7 ms against S6.7's 100 ms. The baseline's exact point path took 27 to 36 ms through Detail alone and 140 to 227 ms through one Presence segment, and 125 to 231 s through three with an estimate miss; no read now walks a stage on the owner or a point worker. The first read of a launch compiles the stack's programs on the tile worker's device, 50 to 80 ms more.

### Export at 24 and 60 MP

`export.jpeg` through the desktop's evidence `export` step, five GPU exports and five reference exports (`reference: true`) alternating, of a trivial stack (Basic exposure +0.3) and then the heavy one (the three-segment stack without the lens: Detail, Presence's three fields, a radial's masked Presence). The wall time is the activity board's `duration_ms` for the export job, which it keeps only for work of 250 ms or more; a shorter export is timed from the step to its end as the desktop's 100 ms job reader sees it, an upper bound. Every GPU export named `renderer` `{record: "gpu"}` and every reference one `{record: "reference", reason: "requested"}`. Executable `3f08f163`; each source run twice, the second, quoted, at a lower load.

| Source | Stack | Load | GPU: median, range (5) | Reference: median, range (5) | GPU tiles an export | Tile worker's peak |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 24 MP JPEG | trivial | 3.35 | under 250; 121 to 123 by the reader | under 250; 119 to 124 by the reader | 6 | 83.9 MB |
| 24 MP JPEG | heavy | 3.35 | 945, 935 to 971 | 763, 759 to 811 | 24 | 723.3 MB |
| 60 MP JPEG | trivial | 2.62 | under 250; 219 to 323 by the reader | under 250; 223 to 229 by the reader | 15 | 83.9 MB |
| 60 MP JPEG | heavy | 2.62 | 10,350, 10,165 to 10,469 | 2,156, 2,113 to 2,223 | 240 | 783.2 MB |

- **The GPU export of a heavy stack misses**: at 60 MP 10.2 to 10.5 s against the reference export's 2.1 to 2.2 s, nearly five times slower, in 240 tiles under that build's 1 GiB tile-worker budget (the current budget is 2 GiB);  at 24 MP 0.94 to 0.97 s against 0.76 to 0.81 s, a quarter slower. A trivial export is under a quarter of a second either way.
- The first runs, at loads of 5.08 and 6.36 with the owner's browser busy, agree: 10.4 to 11.5 s against 2.1 to 2.8 s at 60 MP, and 0.98 to 1.08 s against 1.00 to 1.06 s at 24 MP.

### The first picture on a cold shader cache

Three launches of the three-segment probe, each with a new `LUXFORGE_BACKGROUND_BUNDLE_SUFFIX`, so the bundle's Metal cache starts empty (the system's compiler cache outside the bundle was not cleared), against one launch on the shared, warm bundle; milliseconds from the editor's `startup` event, the Air 2S opened with no edits.

| Run | Load | Reference frame drawn | GPU frame drawn | First warm-up | Its picture's own programs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Fresh bundle 1 | 3.98 | 1,207 | 2,351 | 2,120 ms, 19 sequences | 133 ms |
| Fresh bundle 2 | 4.36 | 1,261 | 2,365 | 2,112 ms, 18 sequences | 151 ms |
| Fresh bundle 3 | 3.81 | 1,226 | 2,550 | 2,240 ms, 18 sequences | 127 ms |
| Shared bundle | 2.35 | 287 | 398 | 33 ms, 12 sequences | 0.4 ms |

- On a cold cache the reference frame is on screen about 0.9 s later than on a warm one and the GPU's 1.1 to 1.3 s after it, against 0.11 s warm. The commits after it on the same cold bundles warmed what they added: Presence's three fields in 0.51 to 0.53 s (the open stack's part 0.34 s), then a masked Presence in 0.88 to 0.89 s, nearly all of it the open stack's part. TASK-009 recorded 1.5 to 4.4 s for the first warm-up and 41 ms for the picture's own programs on the `gpu-preview` scenario's stacks.

### Compare and a history return

An evidence script over the Air 2S with Detail, Presence's three fields and a Basic exposure of +0.3, five of each step, `wait` steps of 2.5 s between them; executable `3f08f163`, load 3.54.

| Step | Frame presented | Picture at rest drawn | Counts |
| --- | ---: | ---: | ---: |
| Compare on (the reference renderer's frames of both sides, by design) | 592.5 to 652.7 | | with the frame |
| Compare off (its retained frames restored) | 19.3 to 21.1 | 1,026 to 1,043 | 1,092 to 1,108 |
| History: an earlier entry (Detail alone) | 8.8 to 9.0 | 76 | 84 |
| History: back to the current entry | 8.8 to 9.0 | 1,040 to 1,064 | 1,105 to 1,131 |

- The baseline re-rendered the exact frame on every toggle and return, about 1 to 3 s on a heavy stack; the photograph now returns within one or two frames and sharpens about a second later.

### A RAW white balance

The Air 2S's Temperature, `--action set-raw --parameter temperature`, no other layers. The drag drew 30 of 30 inputs on the GPU at **8.5 / 9.0 ms** (load 2.46). Its releases, 30 commits of one input each (load 2.50): the redeveloped picture presented 133.8 / 141.8 ms after the release, its picture at rest 255.6 / 276.9 ms, its counts 263.4 / 285.5 ms. The baseline held the window for the redevelopment, 0.5 to 1.6 s. After each release the next gesture's first tick waits for the redeveloped source's boundary (`surface-pending`) and is drawn 8.7 / 9.2 ms after its input. `editor-latency` refuses both runs (a held tick is an input it pairs with no answer), so these figures are paired from the runs' `events.jsonl` exactly as the tool pairs a GPU tick, a pairing that reproduces the tool's own 9.0 / 9.2 ms on the drag stack's Fit run.

### A colour drag under Dehaze

A Basic exposure drag under Presence's three fields at +100 and a full Basic layer (`--basic --presence`), so Dehaze's light is computed on the GPU for every tick from the whole stage at full resolution; 30 inputs each, all on the GPU, executable `3f08f163`:

| Source | View | Load | Input to drawn p50 / p95 | `draft.set` round trip p50 / p95 |
| --- | --- | ---: | ---: | ---: |
| 24 MP JPEG | Fit | 2.98 | 8.8 / 9.1 | 0.2 / 0.2 |
| 24 MP JPEG | 100% | 3.24 | 8.7 / 9.1 | 0.2 / 0.3 |
| 60 MP JPEG | Fit | 3.86 | 9.0 / 9.4 | 0.2 / 0.2 |
| 60 MP JPEG | 100% | 4.62 | 9.0 / 9.3 | 0.2 / 0.2 |

- Every tick is drawn at the first redraw after its input, so the light's per-tick cost fits within a 120 Hz frame at 60 MP; the tools time frames, not GPU passes, so its own duration is not separated. The baseline paid 110 to 420 ms a unit a tick, and 864 ms for the first 60 MP region.

### Idle memory

The performance scenario saw the footprint and RSS grow about 20 MB with every idle capture 5 s apart. The 60 MP JPEG with Detail, Presence's three fields and a full Basic layer, a release editor in the background bundle, sampled every 5 s with `ps` (RSS) and `footprint` (`phys_footprint`):

- **With no captures** (an ordinary `develop --background` launch on a copy of the stack's catalog, no evidence directory, the Performance section open, load 4.38): once the picture at rest had settled, RSS held at 870.9 to 871.0 MiB for two minutes and the footprint moved between 1.970 and 2.010 GB with no trend; the process used 1.31 s of CPU in 117 s, 1.1% of one core, the Performance section's sampler included.
- **With a capture every 5 s** (an evidence script of 26 `wait` steps of 5 s, load 2.11): RSS rose by 20,320 to 20,480 KiB at every capture, which is one 2880 × 1800 window readback of four bytes a pixel (20.7 MB), and fell back by 180 to 200 MiB twice in the two minutes, as the allocator returned the readbacks' pages.
- So the growth is the evidence capture's readback, held until the allocator returns it, not a leak of the editor at idle.

### The timing tier

`verify --tier timing` ran once (executable `d0867b9d`, 379 s) and passed, but every timing component started above the 8.0 threshold, 14.0 to 15.7, raised by the tier's own `check` just before them, so it reports every timing verdict as unreliable rather than as a pass or a miss. Its timing components were therefore run again on their own once the load had fallen below 3.5, with the same executable, at the sample counts the targets name (`measure` at five launches a workload):

| Provisional target | Measured 2026-10-06, alone | Load | Verdict |
| --- | --- | ---: | --- |
| Warm 24 MP slider-to-presented-frame p95 < 16 ms | 8.2 / 9.0 ms, 30 inputs, all on the GPU | 3.45 | **Pass** |
| Settled exact histogram p95 < 200 ms after the final input, 24 MP | 67.7 / 76.0 ms, 30 commits (exposure alone) | 3.50 | **Pass** |
| Burst ≥ 30 presented frames a second; staleness p95 ≤ 50 ms | 109.8 frames a second; 8.0 / 8.4 ms over 339 frames | 3.26 | **Pass** |
| 24 MP working set ≤ 600 MiB, sampled RSS | 336.4 / 436.2 MiB | 2.99 | **Pass** |
| 60 MP peak ≤ 1 GiB, sampled RSS, one open | 498.3 / 723.1 MiB | 2.99 | **Pass** |
| Idle CPU < 1% of one core over 30 s | 1.43%, one window, the Performance section open | 2.99 | **Miss** (one window) |
| Launch to usable empty shell p95 < 1 s warm | 1,869 / 1,893 ms, an upper bound through the background bundle | 2.99 | **Miss** (1,378 / 1,476 ms on 2026-10-02) |
| Uncached 24 MP JPEG to Fit preview p95 < 750 ms | 1,099 / 1,114 ms open to raster | 2.99 | **Miss** (783 / 827 ms on 2026-10-02) |

The launch and open figures are later than the 2026-10-02 record by about 0.45 s and 0.3 s, which this run does not attribute; the GPU-first stages changed the open's first picture to the reference renderer's whole frame ([GPU previews](../design/gpu-preview.md#the-picture-at-rest)).

### Reproducing it

```sh
cargo build --release --locked -p luxforge-app -p xtask
cargo run --release --locked --package xtask -- editor-latency --source AIR2S.DNG --output NEW_DIR \
  --mode drag --samples 30 --basic --detail --presence --warm 5000 [--zoom 33|100]
cargo run --release --locked --package xtask -- editor-latency --source AIR2S.DNG --output NEW_DIR \
  --mode commit --samples 30 --basic --detail --presence [--zoom 33|100] [--reference-renderer]
cargo run --release --locked --package xtask -- editor-latency --source AIR2S.DNG --output NEW_DIR \
  --mode paint --samples 240 --masks 3 --mask-presence --presence --detail [--zoom 100] [--reference-renderer]
cargo run --release --locked --package xtask -- editor-latency --source AIR2S.DNG --output NEW_DIR \
  --mode drag|commit --samples 30 --action set-raw --parameter temperature
cargo run --release --locked --package xtask -- editor-latency --source fixtures/generated/60mp.jpg \
  --output NEW_DIR --mode drag --samples 30 --basic --presence --warm 3000 [--zoom 100]
# Samples, export, Compare and history, idle captures: an evidence script of `api`, `export`,
# `compare`, `preview` and `wait` steps, launched in the background bundle; a fresh
# LUXFORGE_BACKGROUND_BUNDLE_SUFFIX for an empty Metal cache.
[LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=NEW] cargo xtask develop --background --hidden-window \
  --evidence-dir NEW_DIR --evidence-script SCRIPT.json --open SOURCE
```

## GPU throughput against the reference (TASK-012)

The GPU's picture at rest, its counts and its export after TASK-012 of [the plan](../../tasks/rendering/gpu-first.json) (tile charges matching the slot, a larger rest share, tiles ordered by window shape, staged sweeps on the surface and the tile worker, the pipelined tile worker, lights kept across refits, the exact light behind a spatial layer), each row against [TASK-011's figures](#gpu-first-against-the-2026-10-04-baseline) (*before*) and the same build's reference renderer (`--reference-renderer`, which launches with `--no-gpu-render`, or `reference: true` for an export), back to back. The release gate is not run here; it passed on the whole corpus with the exact light ([above](#the-light-behind-a-spatial-layer)).

### Scope

- **Host.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal, a hidden 1440 × 900 window at 2× in a background-only bundle, warm filesystem cache, 7 October 2026, 10:17 to 10:33 and 11:16 to 12:08 BST. The host slept from 10:33:46 to 11:05:09 (`pmset -g log`); no timed run's window overlaps it, the last run before it ending at 10:33:16. From then on each run's window was checked against `pmset`'s sleep and wake records. The host ran on battery throughout, but for 11:29 to about 11:38 on mains power; no low-power mode (`powermode 0` on both). Runs repeated across the two agree within their spread (the 24 MP GPU counts 242 / 264 ms on battery, 226 / 242 ms on mains).
- **Quiet host.** Before every timed run a gate waited until no `cargo`, `rustc`, `clang`, `ld` or Clippy process of another worktree was running and the one-minute load was below 4.0; every run quoted started between 2.5 and 4.0. A reference run raises the load itself (to 7 to 10 by its end), so the run after it waited for the load to fall. Another session built for about a minute at 10:19; the gate held the run until it had finished and the load had fallen. syspolicyd kept pid 15580 throughout.
- **Build.** Source `2e22257d` (`main`, equal to `claude/gpu-first-9b5b17`), release, `--locked`, Cargo.lock SHA-256 `8f7bce2f…`. `cargo build --release --locked -p luxforge-app -p xtask` made `73d43a6d…` (52.0 MB), which every `editor-latency` and `measure` run used (`--binary`); `cargo xtask develop --background`'s build of `luxforge-app` and `luxforge-cli` made `04e72e5e…`, which the evidence-script probes used (exports, the cold shader cache). Launch and open were also measured on `2837c344` (2026-10-02, `37cc1db8…`, 35.5 MB), built the same way in a detached worktree.
- **Sources.** The Air 2S DNG (5464 × 3640, `aab79ce1…`), the X100VI RAF (7728 × 5152, `187e3403…`), the generated 24 MP and 60 MP JPEGs (`b54c2a15…`, `b9e0118a…`), each hashed the same before and after the runs.
- **Stacks.** *The drag stack* and *the masked stack* as [TASK-011 defines them](#gpu-first-against-the-2026-10-04-baseline). *The three-segment stack*: Detail, Presence's three fields at +100 and a masked Presence on a linear mask (`editor-latency --mode commit --detail --presence --mask --action set-presence --parameter clarity`), each commit a new masked Clarity, over the Air 2S as imported; TASK-011's used a radial mask and Texture beside Clarity. *The heavy export stack*: Detail, Presence's three fields and a radial's masked Presence; *the masked export stack*: Detail, a global Presence of Texture 25 and Clarity 20, and three radial masks each holding a masked exposure of +0.3 and a masked Presence of Clarity 50 and Texture 40, radials where the masked stack has a brush.
- **Figures.** Milliseconds, nearest rank, p50 / p95 over 30 samples unless a count is given. *Rest* runs from the release to the surface's first draw of the picture at rest (`gpu_rest_drawn`), *counts* to the GPU's settled histogram (`analysis_adopted`); the reference renderer's frame and its report arrive together, the counts the tool reports. Commit runs time each commit after the previous one's counts have arrived.

### The picture at rest and the counts after a commit

| Row | Before: GPU counts / reference | GPU rest | GPU counts | Reference frame and counts | Load (GPU, reference) | Tiles drawn |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 24 MP JPEG, the drag stack, GPU first | 649.4 / 653.4; 671.9 / 775.8 | 210.1 / 231.9 | 242.4 / 263.6 | 644.4 / 694.6 | 3.67, 3.20 | 2 sweeps of 6 tiles of 2048 px |
| The same, reference first | | 201.6 / 209.9 | 225.7 / 242.3 | 665.3 / 682.2 | 3.99, 3.27 | the same |
| Air 2S, the drag stack, reference first | 816.3 / 825.8; 793.1 / 878.9 | 226.2 / 231.8 | 263.7 / 268.5 | 1,136.5 / 1,188.0 | 3.30, 2.86 | 2 sweeps of 6 tiles of 2048 px |
| The same, GPU first | | 226.9 / 230.5 | 264.1 / 266.2 | 1,152.9 / 1,311.0 | 3.08, 3.38 | the same |
| 60 MP JPEG, the drag stack, GPU first | 5,676.9 (6 commits); 1,682.3 | 565.7 / 577.6 | 601.5 / 613.8 | 1,923.3 / 2,037.3 (28 commits) | 3.36, 3.36 | 2 sweeps of 15 tiles of 2048 px |
| The same, reference first | | 564.1 / 568.1 | 599.9 / 604.0 | 1,763.6 / 1,952.0 (20) | 3.10, 3.52 | the same |
| The same, reference first again | | 574.3 / 585.8 | 610.6 / 621.7 | 2,092.3 / 2,348.0 (20) | 3.99, 3.92 | the same |
| X100VI RAF (40 MP), the drag stack, reference first | not measured | 597.7 / 614.2 | 615.8 / 646.6 | 2,432.2 / 2,564.5 (20) | 3.85, 3.73 | 2 sweeps of 12 tiles of 2048 px |
| Air 2S, the three-segment stack, GPU first | rest 14.9 to 17.3 s (4 launches); reference not measured | 301.9 / 333.5 | 309.4 / 341.2 | 959.7 / 1,022.6 | 2.92, 2.84 | 3 sweeps of 24 tiles of 1024 px |
| The same, reference first | | 297.7 / 317.3 | 305.6 / 326.1 | 976.4 / 1,053.8 | 3.97, 3.87 | the same |
| Air 2S, the masked stack's stroke release, one a launch | 16.99 and 17.05 s; 1.26 s | 672.8 to 700.0 (5 launches) | 706.2 to 732.4 | 1,301.8 to 1,420.5 (4 launches) | 2.51 to 3.99, 2.98 to 3.91 | 4 sweeps, 42 tiles |

- **Every row's picture at rest and counts now land sooner than the reference renderer's frame and report, at p50 and p95**: by 2.6 to 2.9 times at 24 MP, 4.3 to 4.9 times on the Air 2S, 2.9 to 3.4 times at 60 MP, 3.9 times on the X100VI, 3.1 times over the three segments and about 2 times after the masked stroke. The 60 MP row is 9.4 times sooner than before, the masked release 24 times. The reference's own figures moved between runs more than the GPU's (at 60 MP 1.76 to 2.09 s at p50 in three runs); the tool's 60 s deadline let the first reference run at 60 MP finish 28 of 30 commits, paired from its events, and the others were run at 20.
- **Attribution, from `gpu_rest` and `gpu_rest_drawn`.** Each picture at rest is now planned in tiles of 2048 px wherever the share allows, where TASK-011 drew 24 tiles of 1024 px at 24 MP and on the Air 2S, 240 of 512 px at 60 MP and 330 of 256 px over a masked Presence. Behind Detail every Dehaze stack is drawn in at least two sweeps, the first writing the stage texture its exact light is reduced from (`lights_encoded` 1, `lights_restored` 5 to 8, `retirement_waits` 0 to 1 a picture; no light sweep was needed on any row: the stage textures fitted). The tiles' summed GPU spans (an upper bound, the spans overlapping) were 0.23 s at 24 MP, 0.36 s on the Air 2S, 0.39 to 0.41 s over the three segments, 0.64 s on the X100VI, 0.85 to 0.87 s after the masked stroke and 1.04 to 1.06 s at 60 MP, the slowest tile 28 to 61 ms; at 60 MP the picture at rest arrives in about half its tiles' summed spans, which overlap. After the masked stroke the rest names 330 tiles of 256 px for its chained plan and draws 4 sweeps of 42 tiles, with 26 refits and 9 rebinds; the GPU-preview peak was 2,146.5 MB of the 2,147.5 MB budget.
- **The committed frame** stays the GPU's view plan at once: 9.5 to 10.0 ms at p50 and 9.7 to 17.2 ms at p95 on every GPU row.
- **The 60 MP RAW drag stack**, whose stage textures do not fit, so its light is computed by a light sweep: no 60 MP RAW is in the corpus, so it was not measured in the editor (the X100VI's 40 MP stack fitted its stage textures and drew no light sweep). Its indication on the headless surface (`gpu_rest_the_60_mp_raw_drag_stacks_light_indication`, release, two runs, loads 3.16 and 4.03, an indication and not a timing): as the editor plans it, 20 light-sweep tiles of Detail and 70 chained, 1.63 and 1.65 s, the tiles' spans summing 5.40 and 5.41 s; staged at 1024 px 0.99 s; the reference renderer's whole frame 2.57 and 2.55 s (5.31 s in the [earlier indication](#the-light-behind-a-spatial-layer), whose 1.70 to 2.14 s it repeats).

### A developed photograph's catalog tiers

Both tiers (grid 512 px, large 2048 px) of the corpus's Z 6 and Air 2S under a Basic layer and then a Presence layer, drawn by the GPU tile worker and by the reference, each from its own preparation of the original: `a_gpu_tier_of_a_raw_is_within_the_display_limit_of_the_reference_tier` (release, `--ignored`), three runs at loads 3.18 to 3.62, each run drawing the GPU's tiers first, so the order was not reversed. An indication from a test, not the catalog's own lane under the editor.

| Source and layer | GPU, three runs | Reference, three runs | 2026-10-06 indication, GPU / reference |
| --- | ---: | ---: | --- |
| Z 6, Basic | 333, 241, 252 | 423, 415, 421 | 320 / 370 |
| Z 6, then Presence | 552, 341, 399 | 802, 798, 828 | 840 / 690 |
| Air 2S, Basic | 246, 235, 241 | 402, 406, 406 | 440 / 470 |
| Air 2S, then Presence | 346, 334, 338 | 705, 681, 711 | 580 / 680 |

- The GPU draws both tiers sooner than the reference on every row in every run, the Z 6 with Presence in 0.34 to 0.55 s against 0.80 to 0.83 s, where the earlier indication had it slower. Each tier is within its class's display limit of the reference's (largest mean 0.0195, worst block 0.094, p99 0.615).

### Export

`export.jpeg` through the evidence `export` step, five GPU exports and five reference exports (`reference: true`) alternating, after a `gpu_warmed` step; the step is timed from its request to its end, which the desktop now reads through `job.wait`, so it is no longer the 100 ms job reader's upper bound. Beside it, the activity board's `duration_ms` for exports of 250 ms or more. Every GPU export named `{record: "gpu"}`, every reference export `{record: "reference", reason: "requested"}`. Executable `04e72e5e`.

| Source and stack | Before: GPU / reference | GPU: median, range (5) | Reference: median, range (5) | Load | GPU tiles an export; tile worker's peak |
| --- | --- | ---: | ---: | ---: | --- |
| 24 MP JPEG, trivial (Basic exposure +0.3) | under 250 either way | 101, 100 to 110 | 85, 84 to 92 | 2.90 | |
| 24 MP JPEG, heavy | 945; 763 | 275, 269 to 374 (board 258, 251 to 359) | 784, 767 to 819 (board 768, 751 to 801) | 2.90 | |
| 60 MP JPEG, trivial | under 250 either way | 190, 187 to 199 | 175, 167 to 185 | 3.41 | |
| 60 MP JPEG, heavy | 10,350; 2,156 | 629, 622 to 656 (board 613, 606 to 637) | 2,016, 1,944 to 2,247 (board 1,998, 1,928 to 2,222) | 3.41 | |
| Air 2S, the masked export stack | not measured | 457, 451 to 522 (board 441, 433 to 503) | 1,177, 1,149 to 1,191 (board 1,153, 1,132 to 1,174) | 3.31 | 24; 2,112.9 MB |
| The same, a second launch | | (board 467, 451 to 494) | (board 1,170, 1,131 to 1,193) | 3.43 | 24; 2,112.9 MB |

- **A heavy GPU export is now 2.9 to 3.3 times faster than the reference export** at 24 and 60 MP and 2.5 times over the masked stack; at 60 MP 16 times faster than before.
- **A trivial GPU export misses by 15 to 16 ms**: 101 against 85 ms at 24 MP and 190 against 175 ms at 60 MP, every GPU export slower than every reference export. Attributed below ([the trivial export's first band](#the-trivial-exports-first-band)).
- The tile worker's peak was 2.11 GB of its 2 GiB (2,147.5 MB) budget on the masked stack.

### The trivial export's first band

Timed in the process, through the owner's export lane as the desktop starts it (`trivial_export_timing_gpu_against_reference`, release, seven alternating samples after one discarded export of each, the generated fixtures with Basic exposure +0.3): **75 against 57 ms at 24 MP and 158 against 137 ms at 60 MP** (2026-10-08, executable of `5e287c5f`, the M4 Pro at one-minute loads of 5 to 9). The desktop's step timing above reads the end on its frame clock, about 8.3 ms a frame, so it shows the same gap coarser.

- **Where the gap is.** The reference renders the pointwise stack across the pool in 7 to 8 ms (18 ms at 60 MP) and then encodes. The GPU's encoder starts at once and waits for the stream's first band, a row of 2048 px tiles: 26 to 28 ms at 24 MP and 40 to 52 ms at 60 MP. After it, the encoder never waits again. The gap is that first band's latency less the reference's render.
- **What the first band costs.** On the tile worker's thread, timed tile by tile: the band's window of the source uploaded, 4.7 ms for 6000 × 2048 codes; the first tile's wait for the device, 10 to 11 ms, where a later tile of the band waits 1.3 to 2.6 ms; and each tile's readback and copy into the band, about 2.2 ms. A fresh slot does not explain the first tile's wait: keeping the slot across exports left it unchanged.
- **Tried and not adopted:** 512 px tiles (134 ms at 24 MP: per-tile costs dominate) and 1024 px tiles (93 against 84 ms in the desktop's timing, 184 against 168 ms at 60 MP); a first row of 512 or 1024 rows under 2048 px tiles (at most 3 ms: the second band is then late, the worker taking about 21 ms a band against the encoder's 24); reading a band's last tiles back and sending it before the next band's window is uploaded (73 and 151 ms, but a stream drawn without an encoder, which bounds a heavy export, slower by 7 ms for Detail at 24 MP and 12 ms for the 60 MP drag stack, the device idle during the upload); and copying the mapped readback straight into the band (no change).
- **What is left.** The window's upload and the device's first-tile latency, each paid per band on the worker's one thread. Closing the gap needs the next band's window uploaded beside the current band's tiles, or a band's window textures kept for the next band of its shape, both changes to what the runner holds at once ([export](../design/export.md)); not built.

### Ticks, nothing regressed

30 drained inputs each, every one drawn on the GPU, none held; executable `73d43a6d`.

| Drag | View | Before p50 / p95 | Now p50 / p95 | Load |
| --- | --- | ---: | ---: | ---: |
| Air 2S, Basic exposure over the drag stack | Fit | 9.0 / 9.2 | 9.1 / 9.4 | 3.42 |
| The same | 33% | 9.0 / 9.6 | 9.0 / 9.1 | 3.62 |
| The same | 100% | 9.0 / 9.3 | 9.0 / 9.3 | 3.37 |
| 60 MP JPEG, Basic exposure under Presence's three fields (Dehaze) | 100% | 9.0 / 9.3 | 9.0 / 9.3 | 3.45 |
| Air 2S, a masked Presence Clarity drag over Detail and Presence (`--mask --action set-presence --parameter clarity`) | Fit | not measured (the tool refused held ticks) | 8.5 / 8.7, worst 26.3 | 3.25 |

### Painting at 100% over the masked stack

240 positions over the masked stack at 100% on the Air 2S, two launches: **8.1 / 24.0 ms, worst 58.1** (233 drawn, load 3.12) and **8.2 / 22.5 ms, worst 24.5** (235 drawn, load 3.92); before 8.2 / 23.7, worst 50.4. **Still a miss at p95.** At Fit 60 positions drew at 7.8 to 8.0 / 8.6 to 9.0 ms in five launches.

- **Every slow tick does the same work as a fast one** (`surface_frame_drawn`'s `evaluation`, joined to each input by its draft revision): 5 links run, 52 spatial passes, a window of 6.39 MP and 31.9 M link texels, no refit and no rebind. So the tail is not the word and block buffers regrowing (the hypothesis H1 of step 8, which would show rebinds), and step 8's fix does not apply.
- **When they come.** In the first launch the first four ticks, 51 to 58 ms, ran while the warm-up for the 100% view compiled 6 sequences over 392 ms, ending 181 ms into the stroke. Past that, and in the second launch whose warm-up had finished 72 ms before the stroke, the slow ticks come in runs of consecutive positions, 4 to 13 at a time (33 of 233 and 16 of 235), each presented on the third display frame after its input, 22 to 25 ms, with the same work as the ticks around them drawn in one frame. The tools time frames, not each tick's GPU work, so what lengthens those runs, the GPU's own time against the frame or other work queued beside it (the coverage grid each tick asks for), is not separated.

- **Not whole re-evaluations, and not their reach** (2026-10-08, one launch of 240 positions on the release build after `b849b851` with the evaluation's new `incremental`, `whole` and `reached_texels` figures; load 9.9 to 14.0, so the figures place the tail but are not a quiet-host record): **8.0 / 23.7 ms, worst 24.6**, 237 drawn. Every tick of the stroke ran incrementally, the slow ones among them: none ran its links over the whole window. The texels a tick's links reached, 7.4 to 20.7 M of the chain's 31.9 M, are spread the same over slow and fast ticks. The coverage worker's completions fall as often within a slow tick as a fast one (1.26 against 1.28 a tick). A tick is presented 8, 15 or 24 ms after its input, one, two or three frames of the 120 Hz display, and nothing in between. What is left is the tick's GPU time against the frame and the presentation queue behind a missed frame, and the histogram's count pass each tick runs: the per-tick GPU completion time is not yet recorded.

### Launch and the uncached 24 MP open

`measure` at five launches a workload, with the build of this record (`73d43a6d`) and of 2026-10-02 (`2837c344`), interleaved and reversed: this build, the old, the old, this build, then the old and this build again; and once the old build through its own harness (`2837c344`'s `xtask measure`). syspolicyd's pid was the same before and after every run (15580). Each launch is split at the first RSS sample above 1 MiB (the harness's copy of the executable into a fresh bundle and macOS's first check of it), the log clock's zero (the editor's `startup` event, from the launch figure less the capture's elapsed time) and the first `frame_captured`. p50 of the five launches in each run:

| Build and harness | Load | Launch to empty shell | Spawn to first RSS | First RSS to `startup` | `startup` to capture | 24 MP open to raster | 24 MP `decoded` after `startup` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `73d43a6d`, run 1 | 3.54 | 2,071 | 807 | 1,040 | 273 | 1,214 | 254 |
| `2837c344`, run 1 | 3.19 | 1,821 | 580 | 956 | 253 | 1,203 | 229 |
| `2837c344`, run 2 | 2.95 | 1,883 | 643 | 937 | 263 | 1,222 | 236 |
| `73d43a6d`, run 2 | 3.07 | 2,107 | 806 | 1,035 | 259 | 1,211 | 249 |
| `2837c344`, run 3 | 3.43 | 1,866 | 582 | 1,001 | 250 | 1,214 | 231 |
| `73d43a6d`, run 3 | 3.85 | 2,069 | 760 | 1,038 | 271 | 1,253 | 256 |
| `2837c344` through its own `measure` | 3.62 | 1,825 | 584 | 984 | 254 | 1,224 | 245 |
| 2026-10-02's record of `2837c344` | | 1,378 | | | | 783 | |
| TASK-011's record of `54d910a4` | 2.99 | 1,869 | 632 | 910 | 636 (60 MP) | 1,099 | 218 |

- **Most of the launch and open "regressions" are not in either build.** Through today's harness, or its own, the 2026-10-02 build launches in 1.82 to 1.88 s and opens the 24 MP JPEG in 1.20 to 1.22 s on this host, against the 1.38 s and 0.78 s it recorded then: about 0.45 s of each is the host's, the same for both executables, and these runs do not attribute it. About 0.95 to 1.0 s of every launch and of every open's `open_to_raster_ms` lies between the process's first RSS sample and its `startup` event on both builds (`open_to_raster_ms` runs from the editor's creation, before the log clock starts).
- **What this build adds to a launch: 0.19 to 0.25 s.** 0.18 to 0.23 s of it comes before the process's first RSS sample, the harness copying a 52.0 MB executable into a fresh bundle and macOS checking it on first run, where `2837c344`'s is 35.5 MB; 0.04 to 0.10 s is the editor's own start before its `startup` event (the preferences, themes, flags and launch log the plan names; not separated); `startup` to the empty shell's capture is 10 to 20 ms longer. The editor's launch figure is the harness's upper bound, not a foreground activation.
- **The open is the same on both builds**, 1.21 to 1.25 s against 1.20 to 1.22 s. The open's first picture is the GPU's view plan when its programs are warm (`decoded` `reduced: true`, `preview_displayed` naming `gpu`, a render of about 10 ms) 249 to 256 ms after `startup`, 20 to 25 ms later than `2837c344`'s CPU proxy; the picture at rest follows in 6 tiles 325 to 355 ms after `startup`. The capture `measure` takes waits for the picture at rest and its dissolve, so `startup` to capture at 24 MP is 488 to 519 ms here against 286 to 301 ms on the older build.
- **Idle CPU after an open**: 0.0% of one core over 30 s in all three runs of this build, 1.2 to 1.5% on `2837c344`.

### The first picture on a cold shader cache

The three-segment probe on the Air 2S, three launches each with a new `LUXFORGE_BACKGROUND_BUNDLE_SUFFIX`, then one on the shared bundle; milliseconds from `startup`, executable `04e72e5e`, loads 2.51 to 3.27:

| Run | Reference frame drawn | GPU frame drawn | GPU after the reference | First warm-up |
| --- | ---: | ---: | ---: | --- |
| Fresh bundle 1 | 1,328 | 2,165 | 837 | 2,087 ms, 21 sequences, its picture's own 577 ms |
| Fresh bundle 2 | 1,404 | 2,199 | 794 | 679 ms, 6 sequences, 380 ms |
| Fresh bundle 3 | 1,366 | 2,168 | 802 | 686 ms, 6 sequences, 384 ms |
| Shared bundle | 612 | 723 | 111 | 42 ms, 14 sequences, 3 ms |
| Before, fresh bundles | 1,207 to 1,261 | 2,351 to 2,550 | 1.1 to 1.3 s | 2.1 to 2.2 s |

- The GPU's first frame on a cold cache comes 0.79 to 0.84 s after the reference's, against 1.1 to 1.3 s before; the reference's own first frame is 0.1 s later than before. The second and third fresh bundles compiled 6 sequences where the first compiled 21, so the system's compiler cache outside the bundle, which a new suffix does not clear, served the rest; only the first launch is a cold compile.

### The timing tier

`verify --tier timing --binary` with `73d43a6d` ran once (436 s, 12:00 to 12:08) and passed, but its own `check` (342 s) left the one-minute load at 19.5 when the timing components started, past the 8.0 threshold, so it reports every timing verdict as unreliable, as on 2026-10-06. Its figures agree with the runs above: the 24 MP slider 8.66 ms at p95, the settled histogram 76.2 ms, a burst at 110 frames a second with staleness 8.6 ms at p95, the 24 MP working set 332.6 MiB, the 60 MP peak 507.3 MiB, idle 0.03% of one core, launch 2,152 ms and the uncached 24 MP open 1,278 ms at p95. The components were not run again alone: the rows above measure each of them on a quiet host. By those runs the provisional slider, histogram, burst, memory and idle targets pass, and launch (p95 under 1 s) and the uncached open (p95 under 750 ms) miss.

### Reproducing it

```sh
cargo build --release --locked -p luxforge-app -p xtask
cargo run --release --locked --package xtask -- editor-latency --binary BIN --source SOURCE --output NEW_DIR \
  --mode commit --samples 30 --basic --detail --presence [--reference-renderer]
cargo run --release --locked --package xtask -- editor-latency --binary BIN --source AIR2S.DNG --output NEW_DIR \
  --mode commit --samples 30 --detail --presence --mask --action set-presence --parameter clarity [--reference-renderer]
cargo run --release --locked --package xtask -- editor-latency --binary BIN --source AIR2S.DNG --output NEW_DIR \
  --mode paint --samples 60|240 --masks 3 --mask-presence --presence --detail [--zoom 100] [--reference-renderer]
cargo run --release --locked --package xtask -- measure --binary BIN --output NEW_DIR
# Exports and the cold shader cache: evidence scripts of `api`, `gpu_warmed` and `export` steps.
# A trivial export timed in the process, GPU against reference (needs cargo xtask generate-fixtures).
cargo test --release -p luxforge-app --bins -- --ignored trivial_export_timing_gpu_against_reference --nocapture
[LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=NEW] cargo xtask develop --background --hidden-window \
  --evidence-dir NEW_DIR --evidence-script SCRIPT.json --open /ABSOLUTE/SOURCE
```

The picture at rest is read from the run's `app/events.jsonl` (`gpu_rest`, `gpu_rest_drawn`); a launch's first RSS sample from `measurements.json`'s `rss_samples`.

## The per-frame light's reduction factor

The factor f the per-frame estimate twin of [GPU-first](../design/gpu-first.md)'s stage 3 needs (TASK-005 in [the plan](../../tasks/rendering/gpu-first.json)). The twin is to compute Dehaze's atmospheric light every frame from the stack compiled at a block stage, the content stage reduced by f per side, f dividing 16: each f × f cell's mean through the colour run, each 16 px block's cells averaged, and the light selected from the blocks as Dehaze prepares it. The reference averages each 16 px block of the colour run's output instead, so where the run clips, crushes or masks by value inside a cell, the colour of the cell's mean is not the mean of its pixels' colour, and the light moves by that gap. Each candidate light was handed to the GPU plan and the frame drawn with it judged against the reference frame of the view, as the release gate judges the picture at rest. A measurement of pixels, not of time: `gpu_dehaze_reduction_factor_candidates`. No factor of 16, 8, 4 and 2 draws every cell within the limits: at f = 2, 4 of 231 cells are past them, each −3 EV with Whites and Blacks at −100 under Dehaze at −100. At f = 1, where the cells are pixels, all 231 are within them.

### Scope

- **Host and build.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal; the `release` profile test binary built from `2ccfa459` (SHA-256 `d6f16efa…`), 2026-10-05, one run of the 100% cells and one of the Fit cells side by side, on a host shared with other agents' builds at one-minute loads of about 8 to 35. The pixels are deterministic, so the load does not bear on them: each cell's f = 16 light drawn twice drew the same bytes on all 231 cells, and an earlier pair of runs, stopped after 29 cells, gave the same lights and figures for all 205 candidates both had.
- **Sources.** The generated JPEGs written by `cargo xtask generate-fixtures` into a new directory, and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest, each the SHA-256 the [corpus](../../fixtures/preview/corpus.json) records. Every content stage is a multiple of 8 px a side, so no cell is partial below f = 16 and the twin's unweighted average of a block's partial cells never applies here.
- **What was emulated.** Each light was computed on the CPU, test-only (`luxforge_core::qualification::twin_lights`): for each Dehaze layer, the stage its colour run reads — the output of every layer before it but the colour layers directly before it — rendered by the reference at full resolution and reduced to its exact means over f × f cells, unrequantized; the run compiled at that reduced stage, as the twin compiles its stack, and run over the means with the CPU's own units, a masked layer blended by its coverage at the cell and a range mask reading the cell's mean colour; clamped to [0, 1] on the byte path; each 16 px block's cells averaged; and the light selected as Dehaze prepares it. **f = 1** is a control: its cells are pixels, so for a colour run alone its light is the reference's but for the byte path's 16-bit hand-off; it was within 7.2 × 10⁻⁵ of it on every cell, and its frames' figures were the reference light's to within 0.01. Not emulated: the twin's own GPU arithmetic, half-float intermediates and `f32` sums; Detail run at 1/f where it comes before the light — the candidates at each f hold Detail's output exact, its full-resolution output reduced, which the twin would have only if a rest render seeded it, and the **Detail left out** candidates leave it out, which is what the twin at f = 16 nearly computes, Detail at a sixteenth of its scale barely filtering; and an earlier Presence layer at 1/f — behind a global Presence layer, a masked Dehaze layer's stage is that layer's exact output, drawn with its own twin light at the same f.
- **What was compared.** Each light handed to the GPU plan's light planes for its layer (`Qualifier::set_lights`) in place of the light its light link computes, and the frame drawn on the device against the reference frame of the view by the spatial limits, 1.0 / 2.5 / 5.0 / 0.5: at 100%, the region plan from the stack's first pixel layer over the visible region of the owner's largest window (3026 × 1826, or the whole 1440 × 960 Presence fixture), against that region of the reference frame; at Fit, the process-first frame — the whole output stage drawn as 100% region plans over 2048 px tiles, each reading the same light, and stitched — and the reference frame, each reduced to the Fit frame's size (1715 × 964 behind the 16:9 crop; the Presence fixture's 1356 × 762 is drawn at its own size) by an area-weighted average of its linear light. Beside each frame, its light's error against the reference's light, per channel as a fraction of it.
- **Cells.** At 100%, on the seven sources the corpus measures Detail beside Presence on, the stacks of 15 drags draft (Presence over the moderate Detail to every field at ±100; Detail to the corpus's three settings and sharpen stress under every Presence field at ±100; Basic between Detail and Presence at the corpus's settings, ±1.5 EV with contrast, ±3 EV with Whites and Blacks at the same end, and ±3 EV under every field at ±100), its 49 Basic cells among them, and 10 more: a Basic layer under Presence with no Detail before it (the corpus's settings under 50 / 50 / 30; +3 EV with Whites and Blacks +100 under every field at +100; −3 EV with Whites and Blacks −100 under every field at −100); sharpen stress under Dehaze −100 alone; a Basic layer masked by the corpus's luminance range under Dehaze (the corpus's masked settings and +3 EV under +100, −3 EV under −100); and a masked Dehaze layer after a global Presence layer at 50 / 50 / 30 under a Basic layer (the corpus's settings and +3 EV under the masked layer at +100, −3 EV under it at −100): 175 cells, 112 with Detail before the light. At Fit, 7 drags behind the corpus's straightened crop (16:9, 7°) — Dehaze to ±100, every field to +100, Basic under Dehaze at the corpus's settings and at ±3 EV, and Detail under it — on all eight corpus sources: 56 cells, 8 with Detail.

### Results

Cells within the limits at each view and, over both, the largest of each statistic wherever it is, mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / \|signed mean ΔL\*\|; and the light's error, the largest of a cell's three channels as a fraction of the reference's light, the largest over the cells and their 90th percentile:

| Light | 100% within | Fit within | Largest figures | Light error, largest / 90th percentile |
| --- | ---: | ---: | --- | --- |
| The reference's own, the floor | 175 of 175 | 56 of 56 | 0.096 / 1.81 / 1.01 / 0.049 | 0 / 0 |
| f = 16 | 154 of 175 | 52 of 56 | 23.9 / 24.1 / 24.1 / 34.7 | 0.979 / 0.469 |
| f = 8 | 157 of 175 | 52 of 56 | 14.0 / 14.1 / 14.1 / 9.57 | 0.775 / 0.437 |
| f = 4 | 162 of 175 | 54 of 56 | 8.23 / 10.8 / 8.35 / 1.37 | 0.531 / 0.173 |
| f = 2 | 171 of 175 | 56 of 56 | 5.17 / 13.0 / 5.21 / 6.36 | 0.324 / 0.058 |
| f = 1, the control | 175 of 175 | 56 of 56 | 0.096 / 1.81 / 1.01 / 0.049 | 7.2 × 10⁻⁵ / 4.7 × 10⁻⁶ |
| Detail left out, f = 16 | 103 of 112 | 8 of 8 | 1.98 / 9.23 / 4.04 / 2.33 | 0.784 / 0.338 |
| Detail left out, f = 8 | 103 of 112 | 8 of 8 | 1.98 / 9.26 / 4.04 / 2.33 | 0.643 / 0.219 |
| Detail left out, f = 4 | 105 of 112 | 8 of 8 | 1.98 / 9.93 / 3.44 / 2.33 | 0.526 / 0.169 |
| Detail left out, f = 2 | 107 of 112 | 8 of 8 | 1.98 / 10.9 / 2.36 / 2.33 | 0.164 / 0.104 |
| Detail left out, f = 1 | 107 of 112 | 8 of 8 | 1.98 / 12.8 / 2.36 / 2.33 | 0.105 / 0.066 |

- **At f = 2 four cells are past the limits**, all at 100% and all −3 EV with Whites and Blacks −100 under Dehaze −100. Two are the Basic layer masked by the luminance range on the 24 MP and 60 MP JPEGs (mean 5.17 and 5.17, worst block 5.21 and 5.18, signed ΔL\* −6.28 and −6.36, the light 31% and 32% low): the mask reads each cell's mean colour, which on those fixtures' fine vertical detail, alternating at the pixel scale, falls inside the range where the cell's pixels do not, at every factor (23.9 / 24.1 / 24.1 / −34.7 at f = 16). Two are the plain Basic layer on the Air 2S, behind Detail and without it (worst block 13.0 and 7.14, mean 0.18 and 0.19, the light 6% and 5% low): the stack draws a nearly black frame, its light about 0.0075, and the worst block sits in one place at every factor and grows as the light approaches the reference's (9.24 at f = 16, 13.0 at f = 2) while the mean and p99 fall; drawn with the reference's own light, that frame's worst block is 0.86, though single pixels are up to 88 apart.
- **At coarser factors the same kinds miss on more sources.** At f = 4, 15 cells: the −3 EV Basic layer under −100 with and without Detail on the 24 MP and 60 MP JPEGs, at 100% and at Fit, by the signed ΔL\* (up to −0.74); the range-masked +3 EV layer under +100 on the JPEGs and the Air 2S (worst block up to 4.27); the range-masked −3 EV layer under −100 on the Z6 and the Air 2S too; and the four of f = 2. At f = 8 and 16, 22 and 25 cells: also +3 EV with Whites and Blacks +100 under +100 on the JPEGs, with and without Detail, at 100% and at Fit, whose light is white at both factors (worst block up to 4.68), and the range-masked −3 EV layer on the X100VI; at f = 16 also the range-masked layer at the corpus's settings on the zone plate and the Air 2S and the range-masked −3 EV layer on the zone plate. Of the 49 Basic cells behind Detail, 44 are within the limits at f = 16, as [the reduced stage's light](#lights-taken-over-less-than-the-whole-stage) was, 44 at f = 8, 46 at f = 4, 48 at f = 2 and 49 at f = 1. Nothing else misses at any factor: the Presence and Detail drags, whose lights read no colour run; a Basic layer at the corpus's settings, at ±1.5 EV, and at ±3 EV under Presence at 50 / 50 / 30; and the masked Dehaze layer after a global Presence layer, whose second light moved by 2.2% at most at f = 16 and 0.16% at f = 2.
- **Leaving Detail out**, which is what the twin at f = 16 nearly computes, misses five cells at every factor, f = 1 included, all five within the limits at f = 1 with Detail's output held exact: sharpen stress under every field at −100 and under Dehaze −100 alone, on the zone plate (signed ΔL\* −1.15 and −1.18) and on the Air 2S (−2.33 both), and the Air 2S's −3 EV Basic layer behind Detail. Detail itself moves those lights by up to 10.5% (sharpen stress on the Air 2S), whatever the factor. Its other misses, four at f = 16 and 8 and two at f = 4, are the colour run's, which the candidates holding Detail exact miss too. At Fit all eight Detail cells are within the limits with Detail left out.
- **A light's error alone does not decide its frame.** At f = 2 three quarters of the cells' lights are within 0.23% of the reference's; the largest errors are the ±3 EV and range-masked stacks'. A light 58% high at f = 16 (the range-masked layer at the corpus's settings on the 24 MP JPEG) drew a frame within the limits, and a light 6% low drew the Air 2S's miss.

### Reproducing it

```sh
cargo xtask generate-fixtures --output NEW_FIXTURES
LUXFORGE_GPU_CORPUS_OUTPUT=NEW_DIR \
LUXFORGE_GENERATED_FIXTURES=NEW_FIXTURES \
LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
cargo test --release -p luxforge-app gpu_dehaze_reduction_factor_candidates -- --ignored --nocapture
cargo test -p luxforge-core --lib --features qualification qualification
```

The measurement writes `cells.json`, rewritten after every cell: each cell's layers and estimating layers, and for every candidate its lights, their errors, its frame's figures and verdict, the slot's charge and shape, and whether its f = 16 light drew the same bytes twice, with a summary by view and candidate. `LUXFORGE_DEHAZE_VIEWS` (`100%`, `fit`), `LUXFORGE_GPU_CORPUS_SOURCES` and `LUXFORGE_DEHAZE_DRAGS`, comma-separated, measure only those. The core tests hold the emulation: the cell means exact against a per-pixel sum and, at 16, the reduction's own blocks; the twin's light the reference's through a gain at every factor and through clipping at f = 1.

## The light behind a spatial layer

Dehaze's light behind Detail or an earlier Presence layer, computed exactly from that layer's output through the staged sweeps by the owner's decision of 2026-10-06 ([GPU previews](../design/gpu-preview.md#spatial-programs)), where the stand-in with that layer left out drew a masked Dehaze behind Clarity on the Z6 past the limits (worst block 4.8 ΔE00 at 100%, 2.9 at 50%, 4.5 in export). Where the stage textures do not fit, the light is computed by a light sweep with no stage texture, which draws the layers before the light a second time ([GPU previews](../design/gpu-preview.md#spatial-programs)). A measurement of pixels, with one indication of time.

### Scope

- **Host and build.** The `Apple M4 Pro` adapter on Metal, macOS 26.5.2; the tests and the release gate's harness built from `ed4ec944` (the staged light) and `1308f12b` (light sweeps, after main's Standard RAW look was merged) with the documentation uncommitted, 2026-10-07, on a host shared with other agents' builds; the pixels are deterministic, so the load does not bear on them, but it does on the indication of time.
- **The stage-free light.** `gpu_light_the_stage_free_light_is_the_stage_textures_bit_for_bit`: the same photographs, paths and stacks, and a masked Dehaze behind Clarity before a lens warp, each picture at rest drawn in staged sweeps and again planned with light sweeps and chained tiles, on surfaces of their own; and `a_staged_stream_reads_the_light_behind_a_spatial_layer_from_its_stage`, a 1600 × 1000 stage exported and read by a tile worker drawing it staged and by another drawing it chained after light sweeps.
- **The indication.** `gpu_rest_the_60_mp_raw_drag_stacks_light_indication` (ignored, run on demand in release): the 60 MP RAW drag stack over 9504 × 6336 synthetic planes, its picture at rest drawn by the headless surface once to warm, its kept light let go, and again timed, as the editor plans it and in staged sweeps at 1024 px, beside the reference renderer's whole frame; two runs.
- **The light.** `gpu_light_a_light_behind_a_spatial_layer_is_the_cpus_over_its_exact_prefix`: a picture at rest planned in staged sweeps at 256 px tiles and drawn by the photo surface's own drawing on a headless device, and the light its sweep kept read back, over a 1001 × 667 synthetic photograph and the 1440 × 960 Presence fixture, on the byte and linear paths, for a masked Dehaze +60 behind Clarity +60, Dehaze −100 behind a sharpening Detail and a masked Dehaze +50 behind Texture and Clarity +40 with a Basic layer between: against the CPU's preparation (`Dehaze::prepare`'s reduction and selection, in `f64`) over the stage the sweep wrote, the stack before the light drawn whole on the device and held as the stage texture holds it; against the reference render's own light; and against the CPU's light at full resolution over the exact prefix (`twin_lights` at f = 1).
- **The gate.** `cargo xtask gpu-qualification` over the Presence family, 97 stacks at Fit, 33%, 50% and 100%, the new recipe `presence-dehaze-masked-behind-clarity` among them on all seven of its sources, every output kind; then the whole corpus, 46 recipes and 277 stacks (report `target/gate/whole/` in the worktree that built it).

### Results

- **The light** is the CPU's preparation over the stage the sweep wrote within 2.7 × 10⁻⁶ on every case (the existing light tolerance, 2 × 10⁻⁵, gates it), and on the linear path the reference's own light and the CPU's at full resolution within 1.0 × 10⁻⁶. On the byte path the stage texture holds half floats where the reference hands Dehaze a 16-bit frame: up to 1.3 × 10⁻⁴ from the reference's light, reported and not gated, as for a light over the source.
- **The masked Dehaze behind Clarity** is within the spatial limits at every view on every source, the stand-in's misses gone: at rest the largest of the 28 cells is a mean of 0.019, a worst block of 0.271, a p99 of 0.652 and a signed ΔL\* of 0.002, drawn at Fit, 33% and 50% in 2 staged sweeps and at 100% by the view plan reading the light those sweeps kept; its export within the display limit on all 7 sources (largest 0.016 / 0.223 / 0.633 / 0.001), the same bytes twice; its histograms within 0.0084 code; its samples, each read through the light the tile worker's own sweeps computed, equal to the byte on screen at all 25 points of every source.
- **The Presence family**, 388 cells: every picture at rest within its limits (largest 0.058 / 0.509 / 0.807 / 0.049), and every histogram, sample and export of its 97 stacks, with no gap.
- **The whole corpus** after the merge of main's Standard RAW look, 46 recipes and 277 stacks, passes with no gap: all 1,108 cells of the picture at rest within their limits (pointwise largest 0.081 / 0.284 / 0.834 / 0.010, spatial 0.088 / 0.509 / 0.900 / 0.049), eight of them drawn after a light sweep, and every histogram, sample (6,925 points, each equal to the byte on screen) and export (largest 0.121 / 0.509 / 0.897 / 0.049, the same bytes twice) within its limit.
- **The stage-free light** is the staged light bit for bit on every case, and the picture at rest drawn after its light sweep the staged picture byte for byte, codes and counts; the stage-free export and its reads are the staged export's bytes on both paths, behind Clarity, Detail and before a lens warp.
- **The eight cells the stage textures did not fit**, Detail then Presence with Dehaze on the Z6 and the Air 2S at 100% and the X100VI at 50% and 100%, are drawn by light sweeps and within their limits: over those recipes on the three RAWs, 24 of 24 cells (largest 0.037 / 0.306 / 0.900 / 0.000), each light from one light sweep, the 100% cells' view plan reading it kept.
- **The 60 MP RAW drag stack's picture at rest**, an indication and not a timing: as the editor plans it, a light sweep of Detail in 20 tiles of 2048 px then 70 chained tiles, 1.70 and 2.14 s on the headless surface, its tiles' GPU spans summing 5.6 and 6.8 s (an upper bound, overlapping); in staged sweeps at 1024 px, which the headless surface holds beside no view plan, 1.13 and 1.46 s, 2.7 and 3.2 s; the reference renderer's whole frame 5.31 s.
- **In motion**, not gated, the drag's frame over the reduced source at Fit, 33% and 50% is past the limits on 13 of the new recipe's 21 cells there, as on many of the corpus's other spatial cells ([the picture in motion](#the-picture-in-motion)), and at 100%, where it reads the kept light, equals the picture at rest.

### Reproducing it

```sh
cargo test -p luxforge-app -- gpu_light_a_light_behind gpu_light_the_stage_free a_staged_stream_reads_the_light --nocapture
cargo test --release -p luxforge-app -- --ignored gpu_rest_the_60_mp_raw_drag_stacks_light_indication --nocapture
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --families presence
```

## CPU and memory efficiency, measured on the M4

The [efficiency](../design/efficiency.md) work against the code before it, in two pairs that each
differ by one body of that work:

- **The core and RAW tasks.** `946751cc` against `135e720e`: tile slots and the rolling window,
  the Detail and Presence rows, RAW row reads, the u16 demosaic input, the reduced-grid cache and
  hardware SHA-256.
- **The owner, catalog and desktop tasks.** `e7a65286` against `7a890372`: the WAL catalog,
  shared strokes, a brush tick's copies, collapsed sections, job reads and GPU preview blocks.

**Host and build.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal. Everything was built
`release` with `--locked`; the `dist` profile is deferred. Files and the source cache were warm.
Measured 4 October 2026.

**Order and load.** Each workload ran base, branch, branch, base, and most then ran again in the
reverse order (branch, base, base, branch). A difference is quoted only where it held in both
orders. The one-minute load was 2.9 to 8.0 at the start of every run; the host was otherwise
quiet.

**Statistics.** Every p50 and p95 is nearest-rank. Each output's bytes and plane digests are the
same on both sides.

### Presence

`presence_timing` (`cargo test --release --locked -p luxforge-core --lib presence_timing -- --ignored --nocapture`).
Each stack is at +100 over a synthetic textured frame, with a warm source and warm estimates in one
render context, and 10 renders a run. On the branch, Clarity alone and every stack holding Dehaze
read their reduced planes from the store after the first render. Texture has no reduced grid, so
its rows show the kernels and tiling alone. The estimate and reduced-grid stores are deleted since,
so a reference render now reduces its own estimates and computes its own reduced grids, as the first
render of each run here did. The cells are p50 over four runs a side, with each
render's CPU time (p50 × CPU %).

| Stage | Stack | Before | After | CPU time a render, before → after |
| --- | --- | ---: | ---: | ---: |
| 6000 × 4000 | Texture | 145–159 ms | 67–69 ms | 1.4 → 0.85 s |
| 6000 × 4000 | Clarity | 98–110 ms | 53–56 ms | 1.2 → 0.70 s |
| 6000 × 4000 | Dehaze | 60–68 ms | 25–27 ms | 0.58 → 0.26 s |
| 6000 × 4000 | All three | 519–542 ms | 395–425 ms | 6.2 → 3.7 s |
| 10000 × 6000 | Texture | 371–436 ms | 172–180 ms | 3.5 → 2.1 s |
| 10000 × 6000 | Clarity | 310–338 ms | 154–159 ms | 3.8 → 1.9 s |
| 10000 × 6000 | Dehaze | 179–200 ms | 66–77 ms | 1.65 → 0.77 s |
| 10000 × 6000 | All three | 1663–1801 ms | 1253–1312 ms | 20.0 → 11.6 s |

- **Kernels.** Texture alone renders 55–56% faster and uses 40% less CPU time. That is the tile
  slots, the rolling window and the row-slice passes together, which these runs do not separate.
- **The budget.** The spatial budget's high-water mark, every working set and every concurrency are
  unchanged: for example 208.5 MiB, one tile at a time, for all three fields at 60 MP.
- **All three fields.** These run one 1024 px tile at a time with pooled passes. They gain 23–26%,
  at about 40% less CPU time.

### Detail

`detail-performance --case render --samples 30`: Detail (luminance 25, colour 25, sharpening 40)
after Basic +0.5 EV, at full resolution with a warm context. The cells are p50 over four runs a
side.

| Source | Before p50 | After p50 | Before p95 | After p95 |
| --- | ---: | ---: | ---: | ---: |
| 24 MP JPEG | 283–324 ms | 208–227 ms | 299–345 ms | 258–326 ms |
| 60 MP JPEG | 729–804 ms | 530–545 ms | 789–962 ms | 572–609 ms |

The render is 27% faster at 24 MP and 31% faster at 60 MP. One 60 MP run after the change, whose
first half ran beside other work, is left out (765 ms p50). The frames are `e64d4d51…` and
`2d084fec…` both before and after.

### RAW preparation and development

**Cold open.** Measured with `cold_saved_white_balance_preparation_timing`, run with
`/usr/bin/time -l` and 15 observations a run. The clock runs from `catalog.import` to a strict
exact-source preview job, with a new owner each time:

- read, hash, decode and develop at a saved red gain of 1.1 × as-shot, then adopt;
- nothing is rendered.

Peak RSS is the whole process: its catalog setup and 15 cold opens in turn, not one open's peak.

| Source | Before p50 / p95 | After p50 / p95 | Peak RSS before → after |
| --- | ---: | ---: | ---: |
| Nikon Z6 (30.6 MB NEF) | 162.6–164.8 / 164.1–172.7 ms | 119.7–121.1 / 122.8–128.5 ms | 973–976 → 443–445 MiB |
| Fujifilm X100VI (85.1 MB RAF) | 364.8–367.8 / 373.7–404.6 ms | 245.0–250.7 / 246.7–265.7 ms | 2019–2115 → 741–742 MiB |
| DJI Air 2S (41.0 MB DNG) | 204.6–207.6 / 209.1–213.5 ms | 149.1–149.6 / 152.4–176.3 ms | 1051–1056 → 464–466 MiB |

- **Time.** A cold open is 26–33% faster. Hardware SHA-256 of the original, the sized read, the
  writer-faulted development buffers and the u16 demosaic input all act on it; they are not
  measured apart.
- **Peak RSS.** The process peak falls by far more than the float mosaic and the read buffer.
  Developments of the same sizes in turn no longer accumulate retained memory in the process.

**Development.** Measured with `owner_development_timing`: 30 warm developments of the retained
mosaic at as-shot gains, after one warm-up, with `/usr/bin/time -l`.

| Source | Before p50 | After p50 | Process peak before → after |
| --- | ---: | ---: | ---: |
| Nikon Z6 | 35.8–35.9 ms | 36.2–36.3 ms | 551–553 → 458–461 MiB |
| Fujifilm X100VI | 179.2–184.1 ms | 183.3–192.8 ms | 886–888 → 730–731 MiB |
| DJI Air 2S | 30.2–30.3 ms | 30.7–30.8 ms | 452–455 → 453–455 MiB |

- **Time.** A warm development with the sensor sites takes 1–3% longer than with the float
  mosaic. This loop reuses freed regions, so it cannot show the saving of leaving zeroing to the
  writers.
- **Peak.** One development's peak falls by the float mosaic: 4 bytes a site, 90 and 156 MiB. The
  Air 2S's peak is in its DNG corrections and does not move.

The development diagnostics (`sensor_sites_match_the_float_mosaic_on_every_local_sample`) give the
bytes a development holds through its demosaic:

| Source | Sensor sites | Float mosaic | Saving |
| --- | ---: | ---: | ---: |
| Nikon Z6, Bayer RCD | 293,984,256 B | 391,976,960 B | 97,994,240 B (25%) |
| Fujifilm X100VI, X-Trans Markesteijn | 490,839,696 B | 654,446,592 B | 163,611,648 B (25%) |

**Rows through geometry.** `editor-latency --mode commit --crop 7 --samples 20` on the X100VI:
Basic exposure commits under a 16:9 crop straightened by 7°.

- **The settle** (the exact whole-stage render through the resample, plus the histogram) takes
  93–101 ms p50 against 120–132 ms, 24% less.
- **The committed Fit frame** is unchanged at about 19 ms.
- **The desktop's cold open** of the RAF stays about 1 s, bound by platform start-up.

### The reduced-grid cache

The GPU-first stage 5 deleted this store, and `budgets.reduced_planes` with it: each reference
render now computes its own reduced grids. The figures below are the record of `946751cc`, where the
store served the CPU proxy, the settles and the exact frames.

Measured with `editor-latency --action set-presence --parameter clarity` on the 24 MP JPEG. The
layer held Clarity alone, so an amount change kept the cache key. The counters were
`budgets.reduced_planes`, read from the evidence's `resources.read` answers.

- **Hit rate.**
  - Across 30 commits, only the first render at each stage missed: one at the Fit proxy and one at
    the full stage. Every later settle and Fit render read held planes.
  - Across a 30-input drag with the GPU preview off, every tick but the first hit: 348–360 tiles
    read, 12 computed.
  - With the GPU preview on, the drag's ticks rendered nothing on the CPU, so only the settles used
    the store.
- **Time.**
  - Commit to the settled histogram was 59.0–60.1 ms p50 against 98.9–102.6 ms, 41% less, which
    held the faster Clarity kernel as well as the hits.
  - A drag's final settle was 57–61 ms against 105–108 ms.
  - A drag tick with the GPU preview off stayed one frame at p50 (8.8–9.0 ms); its p95 fell from
    17.9–25.8 to 9.5–10.0 ms.
- **Rebuild cost.** The settle that missed and filled took 84–88 ms, about 26 ms more than a hit.
  That is an upper bound: before the change the first settle was also the slowest, for reasons
  common to both builds. Even so, it was faster than any settle before the change.
- **Retained bytes.** 6,490,812 B in two entries: the Fit proxy's Clarity plane, 490,788 B, and the
  24 MP stage's, 6,000,024 B. There were no evictions or refusals within the 64 MiB budget.

### The owner, catalog and desktop work

All on the 24 MP JPEG at Fit; the cells are p50 over four runs a side.

| Workload | Before | After |
| --- | ---: | ---: |
| Basic exposure commit to the committed frame, 30 commits (the WAL catalog with full flushes) | 17.7–18.0 ms | 18.0–18.5 ms |
| The same commits to the settled histogram | 26.5–26.7 ms | 26.4–26.7 ms |
| A 400-position stroke, GPU preview on, position to frame | 7.4–7.5 ms (p95 8.7–8.8) | 7.5–7.6 ms (p95 8.7–8.8) |
| A 400-position stroke, GPU preview off, position to frame | 8.3–8.4 ms (p95 11.2–16.6) | 8.2–8.4 ms (p95 9.5–9.8) |
| The same stroke's `draft.set` on the owner, a tick | 0.038–0.042 ms | 0.031–0.032 ms |

- **Commits.** A commit with the WAL catalog's single full flush reaches the screen within 0.4 ms
  of before. A durable commit on its own is not measured.
- **A brush tick's owner work.** It no longer grows along the stroke: before, `draft.set`'s p50
  rose 14–26% from the stroke's first 100 positions to its last 100; now it is flat. With the
  preview off, the stroke's p95 falls from 11.2–16.6 to 9.5–9.8 ms.
- **The GPU stroke.** It is one frame a position on both builds.
- **Recording limit.** `editor-latency` records 400 positions unless `--samples` says otherwise
  (`PAINT_POSITIONS`).

### Verification

`verify --tier full --manifest` on the merged work passed 61 of its 64 components, rendered, timing, hardening, `raw-authentic` and `raw-editor` on the three manifest sources among them. `raw-panel` failed on all three sources at its `crop-started` step, where the Basic section reads collapsed once the crop starts and the scenario expects it expanded. The same step fails on `946751cc` and on `e7a65286`, before either body of this work, so the failure is not this work's and stays open.

### Outstanding

These workloads were not measured in the efficiency comparison; later GPU-first measurements cover launch, idle, 60 MP desktop work and masked stacks separately and do not establish the efficiency changes' individual contribution:

- launch and idle (`measure`);
- the desktop work at 60 MP and under contention;
- the histogram reducer's own row (`editor-performance`);
- the lens warp drag;
- the history lineage and the live-session writer;
- shared strokes over a long painting session;
- collapsed sections with a large preset library;
- masked spatial stacks.

Skipping the process scan at launch is shown by the code, not by a measurement: only an evidence
launch asks for system information.

## UI themes, measured on the M4

The runtime theme, the theme library and the Appearance tab ([UI themes](../design/ui-themes.md))
against the build before the runtime theme: `7deaafac` (release binary SHA-256 `9f4b4618…`)
against `82476a52` (`6b9ed32f…`), both under Luxforge Dark with no theme stored, as every evidence
run starts.

**Host and build.** Apple M4 Pro (14 cores), macOS 26.5.2, Metal, background hidden-window
launches of `release` builds with `--locked` (Cargo.lock `8ef94c89…`), warm filesystem cache.
Measured 5 October 2026.

**Order and load.** Each workload ran base, branch, branch, base, and the drags again in the
reverse order. The one-minute load was 1.6 to 4.9 at the start of every run.

**Statistics.** Nearest-rank p50 / p95 in ms, every launch's samples pooled.

| Workload | Before | After |
| --- | --- | --- |
| `editor-latency --mode drag`, 24 MP at Fit, input to presented frame (GPU preview, 4 launches, 120 inputs) | 8.18 / 9.23 | 8.16 / 9.12 |
| The same drag with `--no-gpu-preview` (4 launches, 120 inputs) | 8.63 / 9.10 | 8.66 / 9.16 |
| Workspace derivation per message, `last_rederive_ms` of `slider_draft_preview` (the CPU drags, 248 samples) | 0.030 / 0.058 | 0.030 / 0.065 |
| `view()` of the same update | 0.068 / 0.080 | 0.066 / 0.084 |
| `measure`, empty launch to observed frame (2 runs of 5 launches) | 1754 / 1789 | 1778 / 1919 |

The drag and the derivation are unchanged within the host's spread: the style functions read the
theme Iced passes in place of a constant, and a canvas cache's key gains eight bytes. With the GPU
preview on, a drag derives the workspace through `slider_draft_preview` only once per launch, so the
derivation is read from the CPU drags.

Launch to observed frame was about 23 ms later at p50 in both orders. Split at each launch's first
RSS sample above 1 MiB (samples about 55 ms apart), the difference sits before the process starts:
the launcher's share, which includes copying the executable into the background bundle, moved from a
median of 635 to 690 ms with an executable 1.4 MB larger (the themes code, the bundled palettes
and `toml_edit`), while the application's own share, from its first sample to its first frame, held a
median of about 1105 ms in both builds. No theme is stored in these runs, so the launch read does
not open `themes.json`; with a theme chosen it is one bounded document read more, not measured
here. Idle CPU stayed 1.3 to 1.4% of one core.

## Software adapters

The owner decided on 2026-10-05 that a session with no usable hardware adapter first tries the platform's software adapter (lavapipe on Linux, WARP on Windows) and draws through the GPU path on it, once it is measured fast enough that dragging is not badly laggy ([decisions](../decisions.md#gpu-first-rendering), [GPU-first](../design/gpu-first.md#proposals-with-recorded-defaults)). This is that measurement, against thresholds the GPU-first integrator set on 2026-10-06 for the owner to confirm: at 24 MP at Fit, a drag's tick at p95 within 33 ms for a pointwise stack and within 100 ms for a Detail and Presence stack. **Lavapipe misses them**, so it is not adopted (`adapters::SOFTWARE_ADAPTER_ADOPTED` stays off): a pointwise drag's Fit tick is 2.7 to 7.3 times the 33 ms and a Detail drag under Presence 3.5 to 7.2 times the 100 ms.

### Scope

- **Not a typical PC.** An arm64 Linux container on the owner's M4 Pro, through Docker Desktop 29.4.0, whose Linux VM (kernel 6.12.76-linuxkit) has 14 vCPUs and 7.9 GiB; no GPU is passed through. The image is `rust:1.94.0-trixie` with Debian trixie's `mesa-vulkan-drivers` 25.0.7: lavapipe, `llvmpipe (LLVM 19.1.7, 128 bits)`, Vulkan 1.4.305, a `Cpu` adapter, chosen by `WGPU_BACKEND=vulkan` and `VK_ICD_FILENAMES` naming `lvp_icd.json`. The M4's cores are faster than most PCs', so these figures flatter lavapipe; macOS schedules the VM's vCPUs on its performance and efficiency cores as it chooses. WARP and Windows are not measured.
- **Two CPU counts.** Docker's default, every vCPU (`nproc` 14), and `--cpuset-cpus 0-3` (`nproc` 4), so lavapipe's threads and the reference renderer's pool are four, approximating an ordinary four-core machine.
- **The baseline.** The M4's own Metal adapter, natively, through the same benchmark.
- **Build.** Release, `--locked`, commit `885e741e` (the GPU-first integration branch at `c3f0782c` with this benchmark and the fallback); the container's benchmark built in the container. Measured 6 October 2026.
- **Load.** Other sessions share the host. The figures below are each configuration's quieter run: one-minute load 4.5 to 4.8 for Metal, 5.4 to 9.2 for 14 vCPUs and about 5 to 8 for four. A first run of each at loads up to 33 is given beside them where it differs: the two four-vCPU runs agreed within 8%, the two 14-vCPU runs did not, so the loaded 14-vCPU figures are an upper bound.

### What is timed

The drag-tick benchmark (`crates/luxforge-app/src/app/gpu_drag_bench.rs`), an ignored release test, on the generated 24 MP JPEG (6000 × 4000):

- **A tick.** A draft of the stack's dragged slider, one value a tick swept to and fro, planned by the catalog owner as the desktop plans a gesture's tick (outside the timing; 0.01 to 1.3 ms), then drawn through one slot of the photo surface's own drawing on a headless device with the change measured since the tick before, timed from handing the source to the queue's completion, so the GPU's work is counted, not its encoding. 40 timed ticks a view after one untimed tick that waits out the compile and the source's upload; the last tick's frame equals the same plan drawn fresh in every run. **Fit** is the evidence window's bounds, a 1716 × 1144 frame; **100%** is that window's visible region at full scale, 1798 × 1662.
- **The picture at rest at Fit.** The stack's tiles at full resolution reduced to the view, one tile a frame as the surface draws them, after an untimed draw that compiles; a draw past 20 s is measured once, its compile included.
- **Export.** The stack's output stage streamed through the desktop's GPU tile worker, once cold and once warm, the warm figure given (the cold one where it passed 20 s).
- **The reference renderer's whole frame per tick.** The drafted stack's exact 24 MP frame, the frame a session without a GPU draws per tick once the CPU proxy is retired (stage 5), 8 frames a stack.

The stacks are (a) a full Basic layer, dragging Exposure; (b) Basic, a Tone curve and the colour mixer, dragging Exposure; (c) Detail (luminance and colour noise 40, sharpening 50) and Presence (Texture 50, Clarity 50, Dehaze 30), dragging Clarity, and (c′) the same stack dragging Detail's luminance noise reduction, which draws Presence again over Detail's new output; and (d) three radial masks, each with a masked Presence layer (Texture 50, Clarity 50, Dehaze 30), dragging the last one's Clarity. A Clarity drag reuses every spatial output it leaves unchanged, as the editor's slot does.

**Statistics.** Nearest-rank p50 / p95 in ms.

### Ticks

| Stack | Threshold at Fit, p95 | Metal, Fit | Metal, 100% | lavapipe 14 vCPUs, Fit | lavapipe 14 vCPUs, 100% | lavapipe 4 vCPUs, Fit | lavapipe 4 vCPUs, 100% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| (a) Basic | 33 | 1.6 / 1.6 | 1.6 / 1.6 | 84.9 / **89.0** (loaded 149.6) | 138.0 / 144.4 | 208.7 / **214.3** | 315.7 / 319.5 |
| (b) Basic, Tone curve, mixer | 33 | 1.6 / 1.6 | 1.6 / 3.1 | 103.1 / **119.8** (loaded 226.5) | 154.8 / 171.6 | 237.5 / **242.5** | 359.4 / 361.5 |
| (c) Detail and Presence, a Clarity drag | 100 | 1.6 / 1.6 | 1.6 / 1.6 | 89.0 / **95.8** (loaded 260.9) | 137.4 / 143.1 | 206.4 / **210.7** | 316.4 / 321.9 |
| (c′) Detail and Presence, a Detail drag | 100 | 7.7 / 9.2 | 26.0 / 27.5 | 329.1 / **351.2** (loaded 1063) | 958.8 / 1012.9 | 679.8 / **719.7** | 1821.0 / 1870.3 |
| (d) Three masked Presence layers | — | 1.6 / 1.7 | 1.6 / 1.7 | 102.9 / 110.8 | 161.9 / 176.5 | 188.0 / 191.8 | 297.8 / 302.0 |

Metal's 1.6 ms appears to be the floor of a submission and its wait on that device rather than the work's own time: every light tick lands on it.

### At rest, export and the reference frame

| Stack | Metal: at rest / export / reference frame | lavapipe 14 vCPUs: at rest / export / reference frame | lavapipe 4 vCPUs: at rest / export / reference frame |
| --- | --- | --- | --- |
| (a) | 20.5 ms / 0.04 s / 93.0 / 95.6 | 1.77 s / 1.36 s / 112.6 / 119.7 | 3.97 s / 3.04 s / 275.0 / 296.9 |
| (b) | 22.7 ms / 0.05 s / 214.0 / 218.2 | 1.90 s / 1.48 s / 260.6 / 289.8 | 4.33 s / 3.37 s / 632.7 / 644.0 |
| (c) | 469 ms / 0.30 s / 480.3 / 718.0 | 17.1 s / 6.41 s / 1524 / 1854 | 36.4 s (compile included) / 12.5 s / 795.9 / 804.4 |
| (c′) | 468 ms / 0.30 s / 491.9 / 524.7 | 18.4 s / 6.66 s / 1454 / 1657 | 36.6 s (compile included) / 12.5 s / 752.9 / 801.6 |
| (d) | 12.6 s / 3.44 s / 485.6 / 548.8 | past 120 s / 74.1 s cold / 1210 / 1237 | past 120 s / 147.1 s cold / 660.8 / 668.3 |

The reference frame is the whole 24 MP frame, p50 / p95 ms, Metal's column the M4's own cores. The picture at rest of (d) is drawn in 384 tiles, one a frame, against 24 for (c) and 6 for (a) and (b); on lavapipe it outlasted the harness's 120 s hang bound, which ended it.

### Against the thresholds and the other paths

- **Pointwise stacks fail at every CPU count.** (a) and (b) are 2.7 and 3.6 times the 33 ms at 14 vCPUs on a quiet host, 4.5 and 6.9 times loaded, and 6.5 and 7.3 times at four. At 100% they are slower still.
- **Detail and Presence passes only for a Clarity drag with every vCPU on a quiet host.** (c) is 95.8 ms at 14 vCPUs, within the 100 ms by 4%, 260.9 loaded and 210.7 at four. A Detail drag under Presence, (c′), which redraws Presence, is 351 ms at 14 vCPUs and 720 at four, 3.5 and 7.2 times the 100 ms.
- **Against the reference renderer's whole frame per tick**, in the same container: a pointwise tick on lavapipe, which draws the 2 MP Fit view, costs about three quarters of the reference renderer's whole 24 MP frame (89 against 120 ms at 14 vCPUs, 214 against 297 at four), so for these stacks lavapipe buys little over the whole-frame fallback. For a spatial drag it is several times faster than the reference frame, because the slot keeps what the drag leaves unchanged: 96 against 1854 ms for (c) at 14 vCPUs, 211 against 804 at four, and 351 against 1657 and 720 against 802 for (c′). The 14-vCPU reference frames of the spatial stacks are slower than the four-vCPU ones in both runs, which these runs do not explain.
- **Against today's CPU proxy.** The drag a GPU-less host draws today is the CPU proxy at the view's size, measured on the M4 natively with the GPU preview off ([latency](#latency-with-the-gpu-preview-on-and-off), 2 October 2026): 24 MP at Fit, a full Basic layer, presented at a p95 of 18.0 to 25.8 ms, at 100% 11.9 ms, and the X100VI's Texture drag over Presence 47.2 ms. Lavapipe's pointwise Fit ticks, 89 to 243 ms at p95 before any frame is presented, are 3.4 to 13.5 times the proxy's Basic drag, and its Clarity drag, 96 to 211 ms, 2 to 4.5 times the X100VI's Texture drag; the proxy is retired with stage 5 ([GPU-first](../design/gpu-first.md#stages-and-what-each-deletes)), after which the whole reference frame above is what such a host draws per tick.

Whether a GPU-less host should keep a reduced-size CPU frame per tick rather than the whole reference frame once stage 5 lands is the owner's question; these figures do not answer it.

### Reproducing it

Natively, after `cargo xtask generate-fixtures`:

```sh
LUXFORGE_DRAG_BENCH_OUTPUT=/absolute/NEW_DIR cargo test --release --locked -p luxforge-app \
  --bin luxforge app::gpu_drag_bench::drag_tick_benchmark -- --ignored --exact --nocapture
```

It writes `drag-ticks.json` (every tick, the adapter, the host and the environment) and `drag-ticks.md`; `LUXFORGE_DRAG_BENCH_TICKS`, `LUXFORGE_DRAG_BENCH_REFERENCE_TICKS` and `LUXFORGE_DRAG_BENCH_STACKS` (`basic`, `basic-curve-mixer`, `detail-presence`, `detail-presence-detail-drag`, `masked-presence-3`) measure less, and `LUXFORGE_GPU_ADAPTER=software` asks wgpu for the software adapter alone on a host that has a hardware one too.

In the container: an image `FROM rust:1.94.0-trixie` adding `build-essential pkg-config cmake nasm clang libx11-dev libxkbcommon-dev libwayland-dev libvulkan-dev libvulkan1 mesa-vulkan-drivers vulkan-tools jq` (and, for the editor's own launches under Xvfb, `xvfb xauth libxkbcommon-x11-0 libxcursor1 libxi6 libxinerama1 libxrandr2`, without which it panics before its window opens), with `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`, `WGPU_BACKEND=vulkan`, `CARGO_TARGET_DIR=/target` and `CARGO_HOME=/cargo`; the worktree mounted read-only at `/src`, named volumes at `/target` and `/cargo` so the host's `target/` is untouched, and a writable directory for the output. Build with `cargo test --release --locked -p luxforge-app --bin luxforge --no-run`, copy the test executable out, and run it with `app::gpu_drag_bench::drag_tick_benchmark --ignored --exact --nocapture` and `LUXFORGE_DRAG_BENCH_OUTPUT`, once as it is and once with `docker run --cpuset-cpus 0-3`.

## Method

Optimized builds only, with commit, lockfile, OS, CPU/GPU, RAM, display and storage recorded. Report cold and warm runs separately and say which cold is meant. Keep at least 30 samples and never drop failures or tails silently. Measure user event to presented frame, not shader time, and account CPU RSS, cache bytes, GPU allocations and transient copies without double-counting unified memory. Capture idle after all background work stops. No timing gates in CI; CI enforces exactness, deterministic bounds and coverage. VM checks record hypervisor, guest graphics path and software versus accelerated rendering, and never stand in for native timings.

Correctness checks use deterministic fixtures across all EXIF orientations, gradients, patches and fine detail: coordinate round trips, crop coverage, CPU versus GPU differences and export interpretation, including centered Option resizing and angle sweeps that return without cumulative shrinkage. Color and export proofs choose explicit numerical tolerances and viewing conditions.

## Module activation

Logical boundaries and user enablement do not by themselves reduce memory or launch time. After the first external use case is selected, compare the built-in baseline with an activation prototype on the M4: minimal and default configurations, disabled and enabled-but-unused modules, first use, cold and warm launch, RSS, CPU/GPU allocations, idle work and first-use latency, shader compilation and warm-up on both GPU devices, with packaging size reported separately. Processing-module outputs must retain exact CPU reference fixtures and pass required corpus qualification within the declared GPU tolerance. Disabled modules must start no workers and allocate no processor resources. Reject any optimization that changes output or skips a recipe effect. If the benefit is immaterial, keep the simpler built-in implementation; the external-loader requirement remains regardless.

## Growth rules

Catalog opening never enumerates or decodes all originals. Grid memory depends on visible items and cache quotas. Import, hashing, thumbnailing and indexing use backpressure and resumable batches. Switching photos cancels obsolete preview requests, and the app stays interactive during exports and indexing. Introduce tiling with neighborhood halos before promising unrestricted large-image local processing. A cleanly reported resource limit is acceptable; silent exhaustion is not.

## Catalog-edit comparison

Native Apple M4 Pro / Metal, release, 2026-10-07: three matched runs per binary over the owner's
DSC_3187.NEF (4024 × 6048), using isolated copies of the current catalog's thirteen-layer,
three-mask edit. A global Exposure change from 0.32 to 0.33 EV recreates the freshly committed
GPU presentation; the original edit and source remain untouched. The hidden window is 1440 × 900
points at scale 2. Entry is measured from the scripted backslash tap to the verified renderer
readback with both comparison surfaces ready, including capture overhead; this is not scanout
latency or cold-open performance. Each run opens and warms the photograph first. The host-wide
timing gate is held and starting one-minute load is 2.84–4.86, below the threshold of 8.

| Build | Three entry-to-readback observations (ms) | Median (ms) |
| --- | --- | --- |
| Previous release, edited stack rendered again on the CPU | 1448.3, 1552.9, 1443.2 | 1448.3 |
| Retained GPU After plans and source | 728.3, 750.8, 752.2 | 750.8 |

The measured median is 1.93 times faster. The owner's reported five-second delay is not reproduced
as a five-second baseline in these warmed runs. The extra edited-stack CPU render is removed;
the comparison's second GPU surface still evaluates the retained picture. The edited picture
stays visible during that handoff. In all three final runs the After endpoint and restored picture
match the warmed edited picture exactly in the same 1050 × 1420-pixel photographic rectangle,
excluding divider and label chrome. Captured state records the fixed After picture, the Original
Before and the GPU drawing paths at Fit, through 100%, and on exit. A separate native white-balance
check retains the exact After fallback when Before needs another RAW development. The executor
holds one prepared source; attempting to reuse GPU-only After across that change stalls source
residency, so the optimization is limited to shared-source comparisons.

A comparison entered at Fit retains that display detail when magnified; entering at 100% or
above, with changed RAW sensor gains or historical selection, with clipping marks, without eligible
whole GPU content, or near the shared GPU budget keeps the exact reference After fallback. No general editor memory or latency qualification is
implied. Local evidence is `artifacts/editor-repairs-comparison/verified.json`, with command lines,
binary hashes, source hash, load, captures, state and events beside it; RAW source checks are in
`artifacts/editor-repairs-raw-source/validated-app`, and mask/navigation checks in
`artifacts/editor-repairs-masks` and `artifacts/editor-repairs-navigation/selection-checked-app`.
The final comparison eligibility and source-restoration checks are in
`artifacts/editor-repairs-comparison/current-build/verified.json` and
`artifacts/editor-repairs-raw-source/verified.json`; their native runs qualify behavior, not timing.
