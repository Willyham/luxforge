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
| Slider input to presented frame at Fit, warm 24 MP | p95 < 16 ms; acceptable below 32 ms; a miss at or above 32 ms |
| RAW exposure drag at Fit, input to presented frame, Z6 and X100VI | p95 ≤ 50 ms |
| Burst drag (120 inputs per second for three seconds, alternating direction) | ≥ 30 presented frames per second; each presented frame's staleness p95 ≤ 50 ms |
| Settled exact histogram after the last input, 24 MP | p95 < 200 ms |
| Geometry input to presented preview | p95 < 50 ms once the source preview is ready |
| Empty steady-state process memory | ≤ 150 MiB including helper processes |
| 24 MP single-image edit working set | ≤ 600 MiB CPU-resident |
| 60 MP import peak | ≤ 1 GiB process RSS, GPU memory reported separately; an export may exceed it |
| Idle CPU | < 1% of one core over 30 s after background work settles |
| First page of a 100,000-row indexed filter | p95 < 100 ms warm (catalog qualification) |
| Warm adjacent-image Fit preview | p95 < 150 ms on a cache hit (catalog qualification) |

A single float32 RGBA buffer for 60 MP is about 916 MiB, so unrestricted full-resolution float processing needs tiling before it is promised.

## Auto tone

Native Apple M4 Pro (14 cores, 48 GiB) / Metal, optimized release, generated 24 MP (6000 × 4000) and 60 MP (10000 × 6000) JPEGs. The host was shared with other sessions; each run's one-minute load is given with it, and a figure taken above 8.0 is not a baseline.

**Click to entry.** `editor-latency --mode auto-tone --samples 30`, background evidence launches at Fit with a warm filesystem cache: from the input (the Auto button's action; `--shortcut` sends ⌘U through the key table) to the `analysis_action_answered` of the entry it committed, and to that entry's first exact frame. Warm samples reuse the photograph's sample grid, read once by an unmeasured first Auto in each launch; cold samples commit a slightly larger crop first, so each reads a new grid. On the GPU the frame is adopted with the answer; the reference's exact frame arrives reduced to the view after it.

| Source | Renderer | Sample cache | To entry ms p50 / p95 | To frame ms p50 / p95 | Load at start |
| --- | --- | --- | ---: | ---: | ---: |
| 24mp | gpu | warm | 115.6 / 149.4 | 115.7 / 149.5 | 2.47 |
| 24mp | gpu | cold | 554.7 / 649.2 | 554.8 / 649.3 | 1.86 |
| 24mp | reference | warm | 289.6 / 299.7 | 648.2 / 660.8 | 2.75 |
| 24mp | reference | cold | 513.8 / 566.8 | 883.6 / 944.6 | 2.79 |
| 60mp | gpu | warm | 133.2 / 155.1 | 133.3 / 155.2 | 3.21 |
| 60mp | gpu | cold | 732.0 / 803.4 | 732.1 / 803.5 | 2.69 |
| 60mp | reference | warm | 542.9 / 558.4 | 1435.2 / 1463.0 | 3.56 |
| 60mp | reference | cold | 658.5 / 745.1 | 1583.8 / 1724.9 | 4.15 |

⌘U at 24 MP, cold, on the GPU over 10 samples: 376.5 / 393.2 ms to entry, under a load of 7.14. The warm GPU path meets the provisional 250 ms (24 MP) and 400 ms (60 MP) p95 budgets; a first Auto on a photograph, which reads its grid, and the reference renderer's path do not. Each sample retains 9,078,784 grid bytes at 24 MP and 8,173,568 at 60 MP.

**The engine.** The ignored release test `auto_tone_measure_photo_sized_inputs`, thirty samples per case, a neutral Basic prefix; under a load of 5.46 to 8.52. Its timer covers the tile service's queue and grid read, then the solve after it off the tile service's thread (on the test's thread, as the analysis worker runs it; the hand-off itself is not timed); it excludes original decoding and preparation, catalog commit and preview. Cold and warm refer only to the analysis-sample cache; filesystem, device and shader caches are not purged. The model is Basic alone, or Basic then the starting Look at amount 200, which is not monotonic in Exposure and takes the coarse-to-fine search ([design](../design/auto-tone.md#the-solve-auto-tone1)).

| Source | Model | Renderer | Sample cache | Engine ms p50 / p95 | Grid read ms p95 | Solve ms p95 |
| --- | --- | --- | --- | ---: | ---: | ---: |
| 24mp | Basic | gpu | cold | 341.7 / 383.7 | 259.877 | 137.7 |
| 24mp | Basic | gpu | warm | 94.5 / 131.4 | 0.017 | 131.3 |
| 24mp | Look 200 | gpu | warm | 455.7 / 496.6 | 0.015 | 496.5 |
| 24mp | Basic | reference | cold | 107.7 / 110.0 | 11.553 | 98.7 |
| 24mp | Basic | reference | warm | 96.3 / 97.1 | 0.011 | 97.1 |
| 24mp | Look 200 | reference | warm | 453.9 / 469.6 | 0.017 | 469.5 |
| 60mp | Basic | gpu | cold | 816.0 / 841.9 | 715.625 | 126.2 |
| 60mp | Basic | gpu | warm | 121.7 / 122.8 | 0.015 | 122.8 |
| 60mp | Look 200 | gpu | warm | 430.9 / 437.4 | 0.017 | 437.4 |
| 60mp | Basic | reference | cold | 142.8 / 144.1 | 11.607 | 133.0 |
| 60mp | Basic | reference | warm | 133.9 / 134.9 | 0.012 | 134.9 |
| 60mp | Look 200 | reference | warm | 444.1 / 459.1 | 0.017 | 459.1 |

The warm solve through Basic is about 95 ms at 24 MP and 122 ms at 60 MP. The pooled solve is the basis of the [parallelism rule](../engineering/performance-rules.md): the warm solve took 40.5 ms p95 at 24 MP pooled against 220.9 ms with the shared pool held to one thread (`RAYON_NUM_THREADS=1`), with the same values. The 6000×4000 source uses 698,368 points and 9,078,784 retained bytes (8.66 MiB); the 10000×6000 source uses 628,736 points and 8,173,568 bytes (7.79 MiB). Both are within the 32 MiB retained cap. Warm GPU runs draw no tiles. The read's charged scratch peak is 20.06 MiB; the solver's charged scratch is 6.24 / 5.71 MiB respectively. These scopes do not establish a total RSS/GPU memory budget.

Evidence: `artifacts/qual-sweep/timing-3/auto-*` and `timing-2/auto-*-reference` (click to entry) and `artifacts/qual-sweep/timing-4/auto-engine.json` (the engine), each with its binary, source hashes, load and scope. Reproduce with `editor-latency --mode auto-tone` and the ignored test, setting `LUXFORGE_GENERATED_FIXTURES` and a fresh `LUXFORGE_AUTO_TONE_OUTPUT`. [Auto tone](../design/auto-tone.md) records correctness and the remaining owner qualification.

## Code structure consolidation

The static `luxforge-gpu-types` / `luxforge-gpu` split and worker-scoped stream preparation are implemented ([design](../design/code-structure.md)). Native Apple M4 Pro / Metal, release: full verification passes all 77 components, including the complete available authentic-source corpus of 277 recipe/source pairs, with originals unchanged and existing tolerances retained. The four standard timing components are reliable, at one-minute starting load 3.69–4.67.

Measurements are release builds, 30 samples per workload. Filesystem and system shader caches are not purged. Desktop drag preconditions warm for 1,000 ms, commit preconditions settle, and the export worker discards one warmup. Background invisible windows measure the editor’s texture, excluding scanout/compositing. The export trace was taken at a one-minute load of 3.18–3.60; observations at a higher load are unreliable.

The controlled setup workload builds all three candidate grids plus staged/light selection from an already compiled evaluation, with no pixel reads or GPU work. Across JPEG and synthetic developed RAW at both sizes, the complete candidate workload makes one plan preparation and no stack compile; a production request may stop at an earlier strategy. Scoped worker tests independently prove one preparation and no recompile for the selected request.

| Setup domain/size | Setup ms p50 | Setup ms p95 |
| --- | ---: | ---: |
| JPEG 6000×4000 | 0.0209 | 0.0264 |
| RAW 6000×4000 | 0.0180 | 0.0182 |
| JPEG 9504×6336 | 0.0265 | 0.0295 |
| RAW 9504×6336 | 0.0275 | 0.0297 |

The editor stack holds Detail, a full non-neutral Basic layer and all three Presence fields. JPEGs are generated 24/60 MP fixtures; RAW is the authentic Nikon Z6 source. Each run preserves its original hash. Commit-to-frame can show the current processed motion frame before exact counts settle; those are separate costs.

| Source | Drag to frame ms p95 | Commit to frame ms p95 | Commit to exact histogram ms p95 |
| --- | ---: | ---: | ---: |
| jpeg24 | 9.18 | 9.93 | 242.64 |
| jpeg60 | 9.54 | 16.59 | 649.69 |
| nikon-z6 | 9.34 | 10.48 | 310.10 |

The production export band stream is consumed in order with source development, JPEG encoding and durable publication outside its timer. The 20 MP masked RAW stream uses synthetic developed linear planes. Complete JPEG publication and source preservation are separately checked by the native corpus and rendered export journeys.

| Export-stream workload | ms p50/p95 | Charged worker peak MiB |
| --- | ---: | ---: |
| the 60 MP drag stack | 432.70/440.55 | 1212.43 |
| the Air 2S masked stack | 340.61/346.67 | 2023.05 |
| Detail alone at 24 MP | 92.95/98.86 | 622.29 |

All warmed stream samples compile zero GPU programs. Commit preview charge peaks are 1242.41 MiB at 24 MP JPEG, 1953.81 MiB at 60 MP JPEG and 1963.08 MiB on Nikon RAW. These are existing charges, excluding crop/overlay, backend staging and driver/pipeline overhead; CPU scratch, sampled RSS and native GPU observations retain their separate scope. No generalized speedup or total-memory guarantee is claimed.

Linux aarch64 container headless checks with every graphics driver hidden pass, including slow tests and doctests, as does release owner acceptance. GPU tests skip explicitly there. The optimized editor builds and enumerates no adapter with drivers hidden; all 13 software-rendered journeys pass under Xvfb on Mesa llvmpipe (Vulkan, Cpu), including default reference rendering, explicit software adoption, histogram, GPU preview and no-GPU fallback. This is functional VM/software evidence, not native GPU evidence. Native Windows/Linux GPU evidence and authentic 60 MP RAW desktop/export timings remain unavailable; Windows CI is disabled. Those scopes and complete memory accounting remain open.

Evidence is in `artifacts/code-structure-integrated-full-corrected/summary.json` and `artifacts/code-structure-comparison/report.{md,json}`, which retain every sample, workload/recipe, source/binary/lockfile identity, adapter, command, cache scope and host-load trace. Reproduce using the ignored `code_structure_stream_setup_measurement` and `code_structure_export_measurement` tests in release, plus `editor-latency --samples 30 --basic --detail --presence` in drag/commit modes on the same three sources. Use a quiet native host and fresh output directories.

## Window visibility and event-driven monitoring

The [visibility contract](../design/visibility-and-monitoring.md) pauses presentation sampling only
for native minimization or explicit window/app hiding. Visible unfocused or covered windows keep
sampling. Export, capability and catalog readers use bounded authoritative `job.wait` notifications;
background work and final/partial business answers remain active. Windows/Linux native visibility
facts are unsupported and retain ordinary sampling.

Native functional qualification on the Apple M4 Pro, Metal, macOS aarch64:
`verify --tier quick` passes, as do the touched `select`, `resolve-missing`, `develop-picks`,
`export`, `capabilities`, `performance` and `visibility-monitoring` rendered scenarios. Actual
AppKit order-out/order-in, minimize/restore and app hide/unhide callbacks correlate with timer/read
counts and captured state. Hidden resource reads stop, both long-work display timers are absent,
and restoring takes exactly one fresh sample with CPU/GPU rates unavailable until the second.
Photo pixels, geometry, recipes, disclosure preferences and original hashes stay unchanged.
Hidden export success/refusal and capability success/failure/cancellation are adopted. Catalog
first-look, folder-add, Search partial answers, Locate and Batch adoption retain job identity and
are tested independently of the activity board's display progress.

Measurement scope is the optimized `release` editor and the generated 6000×4000 JPEG. The window
is 1440×900 logical points in a background-only bundle: physically ordered in for native visible
facts, transparent and ignoring pointer input, never key or activated. Frames are renderer
readbacks. This qualifies the presentation gate and process cost in that harness; it does not
qualify an opaque foreground window's compositing.

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

The host was shared with other builds and editor workloads, at a one-minute load between about
4.4 and 38; the second held windows were at 4.43–7.21. These are process CPU observations with
native functional evidence, without a generalized quiet-host baseline or a speedup claim.
Rendering, proxy CPU work, preview/decode scheduling, CPU pools and memory targets are outside
this scope.

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
RGB16 JPEG hand-offs shared with Presence, off-owner pixel reads and value-mask input grids. The [Detail design](../design/detail.md)
and [study](../design/detail-study.md) separate delivered code from outstanding image-quality gates.
This section records the [performance-rules checklist](../engineering/performance-rules.md#review-checklist);
it contains no native residency or total-memory qualification.

| Review question | Answer |
| --- | --- |
| Which paths read, hash or decode originals? | Preparation uses the signature-verified source worker and decoded-source cache. Rendering, deferred point reads and overlay grids receive immutable prepared sources. Cache keys hash bounded recipe/mask data, never source pixels. |
| Which full-frame allocations are there and how are they bounded/shared? | JPEG spatial hand-offs can be RGB16 at six bytes per pixel; each buffer is checked against 512 MiB and the uncached byte driver keeps at most two evaluated frames live. RAW spatial frames keep the 1.5 GiB per-buffer limit. One overlay input grid holds at most 8 million cells (six JPEG bytes or twelve RAW bytes per cell, plus an outside bitset), and its temporary grouped indices use four bytes per cell. Row scratch keeps the 64 MiB target; spatial work keeps the shared 256 MiB target and at least one finite oversized tile may progress. Overflowed working-set byte counts fail before allocation. These bounds do not define total process or GPU residency. |
| Do point queries, validation or no-op checks render a frame? | Validation and no-op planning read payloads and stage metadata. A pixel read through a spatial prefix is a GPU tile read by the tile service, or on the reference the prefix's spatial frames materialized once per call. Dense overlay cells share one tile pass; sparse cells evaluate grown windows through the same tile runner, over the frames of any earlier spatial segment. |
| What runs on the owner thread? | Descriptor/recipe validation, compilation, stage calculation, request identity, transactions and replay checks are owner work. Every pixel read is handed to the tile service off the owner; a query's reads share one session, and a mutation parks once per pixel read, its replay checking the entry, revision, draft and verified source before committing. Preview rendering, cache construction and overlay grid evaluation are worker work. |
| Which desktop messages request state, history, previews or uploads? | A mutation makes its normal state/history refresh and one preview request. Value-mask overlays rebuild only when their input/grid key changes. |
| Which timers, polls or subscriptions are there? | None. Detail checks cancellation between levels and bounded row chunks, including within the first tile. Disconnect drops queued and parked pixel reads and cancels the active one. |
| Which unchanged work is cached, with which keys, limits and measurements? | Overlay and thumbnail input grids avoid repeated prefix evaluation when only mask coverage changes. Keys include source/development identity, canonical prefix and referenced masks, geometry, window, sampling dimensions, byte width and mask-input domain as applicable. Each worker keeps one disposable input-grid entry of at most 8 million cells, retaining no evaluation or source. Exact recomputation, key invalidation and reuse are tested; photo-sized hit rates, rebuild costs and retained-byte measurements remain unqualified. |
| What does 24 MP performance report? | Pending a quiet-host distribution. A full 24 MP Detail render's time is in [the kernels together](#the-kernels-together). No latency-budget pass, Presence JPEG regression verdict or total-memory claim follows from the focused functional tests. Native measurements must record build/source identity, host load and sample counts. |
| Which tests prove exactness and sharing? | Detail's independent reference and deterministic production oracle cover coefficients, extended/constant pixels, tile sizes, sampled kernels and serial/pool parity. RGB16 tests cover every threshold, original 8-bit decode identity, the dark smoothing ramp through Basic, full/sample/region/window equality and buffer limits. Deferred-read and input-grid tests check exact prefix values, owner responsiveness, revision fences, dense/sparse equality and cache invalidation. Native rendered/photo qualification and measurement-only tests remain separate evidence. |

### Detail tile kernels

Detail's smoothing runs over whole rows, one tap at a time across the row, the interior through
slices of it and only the columns within a kernel's radius of the stage's edge through the clamped
read, with the halo checked once a pass. A level of noise reduction shrinks all three channels in one
pass, taking each pixel's chroma energy and factor once; it smooths only the channels whose threshold
there or at a later level is not zero, so Luminance alone smooths no chroma and the fourth level no
lightness; it takes the band in the vertical pass and swaps its planes rather than copying them back.
Sharpening whose blur is its guide, as at Radius 1, smooths once ([Detail](../design/detail.md)).
The independent Detail oracle (`modules/detail/oracle.rs`), the f64 fixtures and the Detail tests
hold the output.

`detail-performance`, release, 30 samples a run, its fixed recipe (Detail at Luminance 25, Colour 25
and sharpening 40 under a +0.5 EV exposure), Apple M4 Pro, two runs at a one-minute load of 22 to
57: well past the 8.0 a quotable distribution needs, so these wall-clock figures are not quotable.
The textured source is `detail.jpg` tiled to 6000 × 4000, since the generated 24 MP JPEG is flat
quadrants.

| Source | A point through Detail, first request / first colour-limited tick, ms p50 |
| --- | ---: |
| `24mp.jpg` | 27.4 to 27.5 / 27.4 |
| Tiled `detail.jpg` | 28.4 to 35.5 / 28.5 to 36.6 |

One 512 px tile, serial, in one release binary, at a load of about 28,
ms p50: noise reduction at Luminance and Colour 25 takes 14.2, Luminance 40 alone 7.8, Colour 40
alone 11.8 and the first recipe at a proxy's scale 17.9; sharpening at Radius 1 takes 14.7, and at
Radius 2.3 14.1. The largest remaining cost is the sharpening limiter's 3 × 3 extrema and gradient,
read through the clamped per-pixel index, about half of sharpening, and the same index in the Oklab
conversion and the reconstruction.

```sh
cargo run --release --locked --package xtask -- detail-performance --source fixtures/generated/24mp.jpg --output NEW_DIR --samples 30 --case render
cargo run --release --locked --package xtask -- detail-performance --source fixtures/generated/24mp.jpg --output NEW_DIR --samples 30 --case points
cargo test -p luxforge-core --lib detail::oracle
```

### The 16-bit hand-off's quantizer and Vibrance's hue

The JPEG path's 16-bit hand-offs, a colour segment feeding a spatial layer and every spatial tile's
output, quantize each channel through an exact index (`Quantizer16`): 65,536 bins over the value's
square root, where the code thresholds lie at least 1.055 × 10⁻⁵ apart against a 1.526 × 10⁻⁵ bin,
so a value is compared with at most two thresholds. The index is 128
KiB, built once ([limits](../design/architecture.md#limits)), and gives the search's code for every
`f32` in [0, 1], NaN, the infinities and the signed zeros
(`byte_quantize16_is_the_threshold_search_at_every_f32_in_the_unit_interval`). Vibrance skips
the hue's `atan2` where Oklab's (a, b) lies more than a degree outside the skin-tone band, where the
hue's weight is exactly one, so every weight is the same to the bit
(`skipping_the_hue_outside_the_skin_band_leaves_every_weight_bit_identical`). Release, one thread,
both kernels in one process, 11 samples, a one-minute load of about 6:

| Kernel, source | Time |
| --- | ---: |
| 16-bit quantizer, gradient (ns a value) | 1.34 |
| 16-bit quantizer, `24mp.jpg` | 0.66 |
| 16-bit quantizer, `detail.jpg` | 0.85 |
| ColourAdjust at Vibrance 50 and Saturation 20, gradient, 85% skipped (ns a pixel) | 22.48 |
| The same, `24mp.jpg`, 50% skipped | 23.72 |
| The same, `detail.jpg`, 12% skipped | 19.16 |

A 24 MP JPEG render of Basic, then Presence's Texture, then the Mixer, whose two hand-offs are
16-bit, takes 334 and 405 ms p50 in two runs at a load of 5.5 to 11.

### The kernels together

The Detail tile kernels, the 16-bit quantizer, Vibrance's hue, the masked colour skip and the
linear resample's gate ([Detail tile kernels](#detail-tile-kernels),
[the quantizer](#the-16-bit-hand-offs-quantizer-and-vibrances-hue),
[masked colour](#units-only-where-the-coverage-is-not-zero),
[per-pass thresholds](#per-pass-parallel-thresholds)) together: release `xtask` on the native
Apple M4 Pro, two runs, each holding the host-wide timing lock, at a one-minute load of 6.9 to
13.4. That is past the 8.0 a quotable baseline needs, so these are not baselines.

- **`detail-performance --case render`, 24 MP, 30 samples.** A full render takes 321 ms p50 in
  both runs (p95 340 and 391), about 4.0 s of CPU a render.
- **`editor-performance`, 30 samples, 24 MP and 60 MP.** The Vibrance and Saturation layer takes
  42.9 and 44.3 ms at 24 MP and 95.9 and 98.5 at 60 MP (p50). Its stacks hold no mask, Detail layer
  or RAW.

### Detail RAW residency diagnostic

The background native RAW journeys on the M4 Pro/Metal at 2x density expose a memory investigation
case. These are ten functional captures per supplied photograph, with Detail, white balance and
geometry changes, from the release editor. All three functional journeys pass. Surrounding
host-load observations exceed the 8.0 quiet-host threshold; these are not performance
distributions or residency qualification.
State resources include the process's source,
developments, caches, retained previews and unified GPU allocations; GPU bytes are not added again.

| Supplied source | OS footprint high-water MiB | Maximum captured resident MiB | Maximum captured reported GPU MiB |
| --- | --- | --- | --- |
| Nikon Z6, 4024 × 6048 | 2583.41 | 1909.75 | 348.53 |
| Fujifilm X100VI, 7728 × 5152 | 3301.11 | 2615.39 | 454.14 |
| DJI Air 2S, 5464 × 3640 | 2339.78 | 1820.70 | 319.03 |

The resident column is a maximum over captured resource samples, not a measured process RSS peak.
Even these samples exceed the 1536 MiB RAW investigation target on each source. Attribution,
idle/cache release, transient/backend staging and finished-build measurements remain open;
the per-buffer and scratch bounds above do not constitute a total-memory pass. Correlated states
and captures are in `artifacts/detail-20260930/native-raw-{nikon,fuji,dji}-final/app/`.

## Recorded baselines

Native M4 Pro, release builds, warm filesystem cache, synthetic fixtures. Diagnostic observations, not accepted budgets or cross-platform claims.

### Sample counts for a p50/p95 claim

Every harness command's default run is a functional run: it proves the journey and gives one launch count you can quote, not a distribution. A p50/p95 figure requires an explicit sample count: 30 samples per recipe for `editor-performance`, 30 inputs for `editor-latency` (one launch), and at least 5 launches per workload for `measure` — 5 gives a median and a maximum, not a stable p95, so use 30 launches per workload for a p95 claim. Every recorded figure states the count it was taken with. Every p50 and p95, in `xtask`'s timing tools and in the crates' own ignored timing tests alike, is read from one nearest-rank `Distribution` (`luxforge-testbase`): `sorted[ceil(percent·n/100) − 1]`, always one of the samples, never interpolated. Some figures below were taken with a tool's or test's own definition and can read one rank apart from the same measurement taken through `Distribution`. The `measure` medians and p95 figures used an interpolated median and an uncapped `p95 = sorted[n·95/100]` index, so an even-count median differs most. The crates' timing tests mostly used the upper median `sorted[n/2]` (or `sorted[round((n−1)/2)]`), which at an even count is one rank above nearest-rank's p50: `presence_timing` and `spatial_timing` at 10 runs, `capability_timing` at 30 and 200, the core's shared-pool Fit-proxy contention figures at 30 (theirs was `sorted[ceil((n−1)·q)]`), `resources_cost` and the process sampler's `cost` at their even counts, and the preset inspection timing at 20. Their p95s, and every figure at an odd count, read the same either way.

| Measurement | Result |
| --- | --- |
| S0 viewer launch to observed frame (empty / 24 MP / 60 MP) | median 233 / 296 / 412 ms |
| S0 request to captured frame (24 / 60 MP) | median 237 / 361 ms |
| S0 sampled peak RSS (empty / 24 / 60 MP) | 111 / 506 / 772 MiB; 965 MiB after sixteen 60 MP loads |
| S0 idle CPU after settling | 0.033% of one core over 30 s |
| Core import of a 24 / 60 MP JPEG | 55 / 118 ms |
| Core one transform on 24 MP after the module registry (p50 / p95, 20 samples) | 12.2 / 13.5 ms; 200 composed transforms 11.4 / 11.9 ms; registration of the built-in modules 0.18 ms and first render after open 0.05 ms on the 480×320 fixture (release acceptance run) |
| Editor RSS after M1/M2 journey with a small fixture | about 101 MiB, 0.2% CPU idle |

### Host and build for the current JPEG editor baseline

Native Apple M4 Pro (14 cores), 48 GiB RAM, macOS 26.5.2 (25F84), Metal on the `Apple M4 Pro`
adapter, a 2880 × 1800 physical window at 2× scale; files on the internal APFS SSD. Release builds,
`--locked`, background-only launches, warm filesystem cache without an OS cache purge. Application
`--locked`, background-only launches, warm filesystem cache without an OS cache purge. Fixtures are
`fixtures/generated/24mp.jpg` (6000 × 4000) and `fixtures/generated/60mp.jpg` (10000 × 6000). The
host is shared with other sessions: the one-minute load
average was between 2.3 and 5.7 at the start of every run recorded below, and the `editor-performance`
runs themselves drive the shared Rayon pool across all cores, so these are diagnostic samples on a
live machine, not a quiesced benchmark.

### Core render, one run, 30 samples per recipe

`editor-performance` on 24 and 60 MP, 30 samples each, release, warm cache. Every row below comes
from **one** invocation per size, so the rows are directly comparable to each other. This is
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
product does not take; they await re-measurement.

The crop output stage measures 3695 × 2077 from the rotated 4000 × 6000 input at 24 MP and
5542 × 3116 from 6000 × 10000 at 60 MP. Against the colour baseline on the same run, each unit's own
added cost at the median is about 15 ms (24 MP) and 42 ms (60 MP) for Exposure's single multiply,
48 ms and 127 ms for Exposure plus the three composed Tone curve stages, 73 ms and 204 ms for the
fused Oklab `ColourAdjust`, and 21 ms and 58 ms for the one composite 3 × 3 linear-sRGB white-balance
matrix. The neutral picker is not a frame operation: it evaluates 25 point samples of the stage the
Basic layer receives at `O(layers)` each and allocates no frame, so its cost does not grow with the
pixel count. `ColourAdjust`, which fuses Vibrance and Saturation into one Oklab round trip, is the
most expensive single unit.

### The masked colour primitive, its own run

A masked colour layer costs the units it would cost unmasked, plus one coverage evaluation and one
blend per pixel **inside the mask's bounds rectangle**, and nothing at all outside it; inside the
rectangle its units and blend run only where the coverage is not zero
([below](#units-only-where-the-coverage-is-not-zero)). The rectangle does **not** remove the pass's
decode and quantization of the rows it touches, because the frame must still be written: skipping a
whole row chunk when every operation in its run is masked and the chunk lies outside every rectangle
is possible and is not built. The unit-evaluation claim itself is asserted rather than inferred, by a
counting colour unit in
`render::mask_tests::a_masked_operation_evaluates_no_unit_outside_its_bounds`, which requires the count to
equal the rectangle's pixels whose coverage is not zero.

`editor-performance` on 24 MP, 30 samples, on a host shared with other sessions: colour baseline
33.1 / 38.3 ms and one `+1 EV` Basic layer 40.3 / 45.5 ms. The unmasked path's arithmetic is the
same with or without masking, proved byte-identical by the colour tests, and the mask is consulted
once per operation per row rather than per pixel.

#### Units only where the coverage is not zero

Inside its bounds rectangle a masked colour operation evaluates each pixel's coverage first and runs
its units only over the stretches of pixels whose coverage is not zero, or whose input holds a −0.0 or
a value that is not finite ([masking](../design/masking.md)); a pixel it skips keeps its input, the bit
the blend gives there, so every output is the same to the bit (`a_pixel_whose_coverage_is_zero_runs_no_unit_and_keeps_its_bits`,
`a_render_that_skips_uncovered_pixels_is_identical_and_samples_equal_it`).
`render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame` times one exposure unit and a full
Basic layer over a programmatically filled 6000 × 4000 frame, unmasked and under four masks, and
prints the process's CPU time beside the wall time and the least of 30 single-thread passes over 64
rows. Release build, Apple M4 Pro, a shared host at a one-minute load of 21 to 62, which moved the
wall times by up to four times: the CPU times and the single-thread figures are the stable ones. Two
runs:

| Case, 24 MP | Non-zero coverage | CPU ms per render | Single thread, ns per pixel |
| --- | ---: | ---: | ---: |
| Basic, bright band (80–100) | 14.95% | 883 / 894 | 24.8 / 24.8 |
| Basic, middle band (25–38) | 25.40% | 1,066 / 1,075 | 31.6 / 31.6 |
| Basic, whole-frame gradient | 100% | 1,672 / 1,678 | 53.8 / 53.9 |
| Basic, unmasked | – | 1,496 / 1,497 | – |
| Exposure, bright band | 14.95% | 675 / 681 | 18.2 / 18.3 |
| Exposure, whole frame | 100% | 290 / 294 | 4.80 / 4.81 |

- **A band mask over a full Basic layer** takes 883 to 1,075 ms of CPU, against 1,496 unmasked, and
  78 to 114 ms of wall time. On the generated `24mp.jpg` (`LUXFORGE_MASKED_COLOUR_SOURCE`), whose
  four flat quadrants a band selects whole, one pair at a load of 21 to 24 took 877 and 890 CPU ms.
- **A single exposure unit** costs too little for the units skipped to show: the band's cost is its
  coverage's luminance encode at every pixel of the stage.
- **Coverage that alternates every pixel**, one-pixel stretches, is the worst case: one exposure unit
  takes 12.4 to 14.2 ns a pixel.

```sh
cargo test --release --package luxforge-core --lib render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
LUXFORGE_MASKED_COLOUR_SOURCE=fixtures/generated/24mp.jpg cargo test --release --package luxforge-core --lib render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
```

### The masked spatial primitive, one to sixteen layers

A masked spatial layer costs what it would have cost unmasked, plus one coverage evaluation and one
blend per pixel of the tiles the mask's bounds rectangle reaches, minus the whole unit chain of every
tile it does not. Each spatial layer, masked or not, is a stage boundary and therefore a **sequential
full frame** in the reference renderer's frame and in its export. A recipe holds at most 16 masked Presence and
Detail layers between them ([decisions](../decisions.md#gpu-previews)), and the
host refuses a seventeenth with a `resource-limit` error naming the limit.

`cargo test --release --locked --package luxforge-core --lib -- --ignored masked_spatial_timing
--nocapture` (`render::spatial::tests::masked_spatial_timing`), on in-memory synthetic frames
rendered by the core alone, warm source and warm estimate store, one `luxforge.presence` Clarity
`+100` layer per mask: the CPU's exact render, which an export runs before it encodes. "Whole frame"
is a gradient whose bounds rectangle is the entire stage; "right-edge band" is one confined to about
a tenth of the columns. Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, release build; one-minute load
2.5 at the start, rising to 12 during the run as the render drives the whole Rayon pool. p50 / p95
of 5 runs, milliseconds:

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
  counters read from the host (`masked_tile_counts`).
- **A whole-frame mask** adds a coverage evaluation and a blend at every pixel: 130 against 94 ms at
  24 MP and 378 against 299 ms at 60 MP at one layer. A masked tile holds one extra tile-sized plane,
  the snapshot the blend is against, which puts the spatial budget's peak at 255.3 MiB at 24 MP and
  239.7 MiB at 60 MP and the 60 MP plan's concurrency at 7 tiles rather than 8.
- The blend is **in place** in the last unit's planes. Copying the tile out and blending into a
  second buffer measured 4464 ms against 1873 at 60 MP — a 2.4× overhead from two fresh tile-sized
  allocations per tile, not from arithmetic. That is the trap: the blend is cheap and the
  allocations are not.

On the **RAW linear path** each spatial operation materializes one `f32` frame, and each frame
replaces the one before it, so at most two exist at once whatever the number of masked spatial
layers, as on the byte path.

### The masked spatial primitive, tiles the mask leaves uncovered inside its bounds

A masked spatial layer copies every tile whose coverage is proved zero, inside the mask's bounds
rectangle as well as outside it, rather than running the unit chain over it: a mask that reads no
pixel is proved on its field alone, one that reads pixels on the tile's own input. The copy is
byte-identical to running the tile (`a_tile_whose_coverage_is_zero_is_copied_bit_for_bit` and
`a_render_that_copies_uncovered_tiles_is_byte_identical_and_reads_equal_it` in
`render/spatial.rs`), and the exact counters `masked_tile_counts` report how many tiles were copied
and how many ran. The saving at photo size is not currently measured.

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

**Scope.** These are the reference renderer's whole-frame renders on the calling thread; a stroke
is drawn on the GPU ([painting](#painting-over-masked-spatial-layers)). The 64-stroke row is a worst
case by construction — sixty-four full-width strokes is far past the point where a second component
is the better answer — and it is the per-component limit rather than a recommendation.

### Masks in the proxy phase

The proxy measured here is the CPU proxy, which draws a drag only in a session without a GPU
([instant previews](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu)); the
desktop drags below are drawn by it. A masked recipe is proxy eligible by construction: mask geometry is stored normalized, so the mask
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

#### An end-to-end masked slider drag

`editor-latency --mask` draws a linear gradient through `mask.create-linear`, enters Mask mode and
opens the mask **by the name the host gave it**: the evidence script resolves a `{"name": …}`
reference against the `mask.list` answer the desktop holds, because `mask.create` assigns the
identity. From the selection on, every generated
slider gesture carries that mask, exactly as the panel's own drag does. With `--basic` beside it the
measured stack holds a global full-Basic layer *and* a masked Basic layer, so each frame runs the
module's whole colour pass twice, once of it through the masked colour primitive.

Eight runs, 30 samples each, drag mode, at Fit, taken back to back and then reversed —
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

**The first run of the sequence is discarded and is shown as `—`.** Every one of its figures,
including the hop, which no amount of render load moves, was roughly double every other run's. It
was the first launch after the machine had been idle, so it paid for cold shader, filesystem and
allocator state; the identical run at the other end of the sequence read 16.93 ms. That is a
finding about the harness and not about masking: **the first `editor-latency` launch after an idle
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
everywhere.

#### A masked Presence slider

`editor-latency --action set-presence
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
target at the median by about a millisecond. This is the proxy phase; the exact phase behind it is
the core table above.

`editor-performance --samples 30` on the same host, release, warm cache, on `24mp.jpg` (load
average 8.6 rising to 17.3) and `60mp.jpg` (load average 17.3 rising to 20.8), **passes every
check**, including that the catalog reopen reconstructs the original historical state and the
source SHA-256 is unchanged, so the phase leaves the core's own correctness diagnostics intact.
Its timings are not quoted: the
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
the load the host was under. Twelve positions at a 24 ms interval — a little over the masked-drag
median of 16.8–17.9 ms, so each position has a round trip of its own to finish — painted
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
conservative rectangle, by [P13](../design/range-study.md#proposals), so those three layers are
evaluated at every pixel with no span skipped — the cost the range study measured at 28–44 ns per pixel
over 100% of the stage, against a placed gradient's 14.6–20.0 ns over 40%. The bare-recipe
measurement below shows that this recipe is not where most of the figure goes.

#### Bare recipe at photo size, with phase attribution

`editor-latency --mode paint` opens one generated photo-sized JPEG, prepares a single brush mask and
one masked Basic exposure layer, then sends a stroke at 24 ms intervals. The rows in this section
use 30 positions along a straight sweep with a hard-edged
brush (feather 0), which takes the mask field's 2 × 2 proxy supersample on every frame and decimates
to its two ends; the workload's feathered, curved stroke is measured
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
round trips also have a several-millisecond tail on 24 MP. This does not justify SIMD, assembly or
changing the mask renderer. Keep the bounded queue and cancellation rules while
tracing whether the remaining gap comes from the active-job handoff or the desktop event loop.

#### Zeroed frames and an identity pass's source rows

A byte frame is an `Arc<Vec<u8>>` allocated with `vec![0; len]`, which the system allocator serves with `calloc`, so no thread fills it before the pass that writes it; an identity pass over the shared source loads the source's rows inside its parallel pass (one `copy_from_slice` per row) rather than copying the whole source on one thread first. `editor-performance --samples 30` on the generated JPEGs, release `--locked`, in-process on the native Apple M4 Pro, holding the host-wide timing lock: four runs per size at a one-minute load of 13.5 to 19.6 (24 MP) and 19.1 to 28.5 (60 MP) from other sessions' builds, so these are not baseline figures. Each cell is the p50 of each run in the order the runs went; the p95s followed the load and are in the evidence, not here. Exposure-only is one +1 EV Basic layer on the upright source and nothing else; one exact transform is a quarter turn.

| Render (p50 ms) | Four runs |
| --- | --- |
| 24 MP Exposure-only | 7.59 · 9.24 · 6.66 · 12.81 |
| 24 MP one exact transform | 6.27 · 15.00 · 6.31 · 6.70 |
| 60 MP Exposure-only | 19.89 · 19.65 · 25.25 · 24.45 |
| 60 MP one exact transform | 14.32 · 16.32 · 13.32 · 25.64 |

The allocation itself explains why the gain is small in a warm loop. A release probe of a 240 MB frame (60 MP RGBA), 14 writer threads: in a fresh process, collecting `repeat_n(0, len)` into an `Arc<[u8]>` is a `malloc` and a serial `bzero` of 10.8 to 12.5 ms, and 12.2 to 13.7 ms with the parallel write after it; `Arc::new(vec![0; len])` returns in 2 to 3 µs, and the parallel write that faults its pages in takes the whole to 8.4 to 9.8 ms (10 processes each). At 96 MB (24 MP) the fresh-process totals are 4.7 to 5.5 ms for the fill and 6.3 to 6.9 ms for the zeroed allocation, so faulting pages in from every writer at once does not pay at that size. Repeating the same allocation in one process, as `editor-performance` does, the allocator hands back the region the last frame freed and both forms cost 1.0 ms to allocate and 2.0 ms with the write (p50 of 30). A zeroed allocation therefore matters most on a first render at a new size, and the identity pass gains by not copying the source serially.

#### Mask feedback and the coverage handoff

Native Apple M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, Metal, release `--locked`.
Hidden background bundles, 2880 × 1800 physical pixels at 2×. The generated
24 MP JPEG is at Fit; the 60 MP JPEG is at 100%. Each launch paints 240 positions at 24 ms on
the harness's sine path, radius 0.06, feather 50, flow 100, with the selected mask's tint on.
The path is decimated as an ordinary stroke is, and each stroke commits one history entry.

Two runs per size, serialized under the timing lock with no local build or test overlapping. A
launch waits for one-minute load below 8 (6.75 to 7.09 at 24 MP, 6.62 to 6.76 at 60 MP); the
second 60 MP run ends at 14.18. One-minute load below 8 cannot guarantee a quiet stroke. Every
run and tail is retained.

The cells give both runs, in their own execution order. Raw inputs,
accepted draft revisions, all distributions and resource samples live in
`artifacts/mask-perf-20261002/wake-abba-*`, summarized by `comparison.json`.

| Metric | 24 MP | 60 MP |
| --- | --- | --- |
| Photo adoption count | 239 · 239 | 239 · 234 |
| Authoritative coverage count | 239 · 239 | 239 · 205 |
| Photo adoption p50, ms | 8.30 · 8.30 | 7.75 · 8.92 |
| Photo adoption p95, ms | 16.23 · 15.75 | 15.12 · 36.12 |
| Authoritative coverage p50, ms | 8.33 · 8.50 | 7.84 · 8.94 |
| Photo event to overlay adoption p50, ms | 0.020 · 0.020 | 0.018 · 0.021 |
| Photo event to overlay adoption p95, ms | 0.025 · 0.025 | 0.024 · 15.634 |
| Preview worker p50, ms | 4.42 · 4.46 | 1.33 · 2.31 |
| Last-quarter preview worker p50, ms | 5.27 · 5.51 | 1.28 · 1.23 |
| Sampled peak process RSS, MiB | 649.8 · 661.3 | 1181.6 · 1133.6 |

The worker returns shared painted RGBA, so the UI thread paints no coverage and the median overlay
handoff is 18–21 µs. The exact palette, support rectangle and contiguous brush index preserve
coverage while reducing work. Both 24 MP runs deliver all 239 post-press positions. Sampled RSS is
a scoped process observation, including GPU resources and allocator retention, rather than a
CPU-heap or whole-editor memory guarantee.

The clean 24 MP photo
median is about 8.3 ms, and one p95 narrowly exceeds the provisional 16 ms target.
The clean 60 MP run is 7.75/15.12 ms p50/p95, but its loaded partner reaches
8.92/36.12 ms and delivers fewer positions. Pre-result scheduling and delayed coverage remain
visible in the raw distributions. Coverage wakes before a matching photograph are suppressed;
publishing photo content before draining coverage prevents lost wakes, while unchanged-photo
feedback and unavailable outcomes stay immediate. The tests prove that liveness; the timings do
not isolate this policy's cost or saving. These are adoption timestamps, not GPU completion or display
scanout. The synthetic brush workload does not qualify RAW, high-density recipes or general mask
latency.

The [mask performance design](../design/mask-performance.md) records the numerical and
memory contracts. Exact brush/range/reference, cancellation, draft, source-preservation, bounded
handoff and wake/publication checks pass; native `mask-interactions`, `mask-brush`, `mask-range`,
`viewport-region`, `performance` and `capabilities` frames correlate state and logs. Twelve native
Metal surface tests pass, with two separate diagnostic timing probes left out.

#### Per-pass parallel thresholds

Each rendering pass kind runs on the shared Rayon pool from its own threshold (`render::limits::parallel_pixels`, [performance rule 9](../engineering/performance-rules.md#rules)), chosen from `render::parallel`'s `parallel_break_even_per_pass`: one unit per case through the built-in modules, serial against pooled (forced on the rendering thread) at 0.025 to 2 MP of the pixels that pass's gate counts, the two ways alternating sample by sample. Release test build on the native Apple M4 Pro, not holding the timing lock, while other sessions built: four runs of 21, 31, 41 (the spatial cases only) and 31 samples per way, at one-minute loads between 7.5 and 44.4. Under that load a pooled run is the one that suffers, because a pool worker the scheduler has parked holds the join: pooled medians wander by several times between runs at every size, while serial medians stay within a few percent. The table therefore gives the pooled/serial p50 of the second run, which covers every case, and for the spatial cases the p50 and, after the slash, the ratio of the fastest runs from the third.

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

Each threshold is the smallest size at which every case of its kind won in every run: the transform 0.5 MP (1.24 to 1.26 at 0.25 MP in all three runs that measured it); one or two colour units 0.1 MP (bytes 0.89 to 0.93 at 0.05 MP); three or more units or a mask 25,000 pixels, the smallest size measured (a mask stays in this group and was not measured on its own); the resample 0.1 MP of output (1.14 at 0.03 MP; its fastest runs 0.46 at 0.07 MP); a spatial operation 0.25 MP of stage (at 0.1 MP Texture's fastest runs lost in the fourth run, 1.67, and Dehaze's broke even, 0.97 to 0.98); and the proxy 0.5 MP of source read, where up to 0.25 MP the downscale is one band and pooling changes nothing. A serial run at 0.025 MP often reports several hundred percent of one core: that is the pool still spinning down from the pooled run before it, which the CPU sampler charges to the process. RAW development, a spatial operation's global-estimate reduction, the analysis reducer and the overlays keep the shared one-megapixel threshold and were not measured here. Serial and pooled write the same bytes at a size between the shared threshold and a pass's own for every kind on both pixel domains (`a_pass_between_the_shared_and_its_own_threshold_pools_to_the_serial_bytes`).

The Lens/Perspective `Warp` pass currently uses `PARALLEL_WARP_PIXELS = 100,000` output pixels, provisionally equal to the resample threshold. Its kernel has no measured break-even yet. `LUXFORGE_BREAK_EVEN_CASE=warp` selects the six serial/pool cases (Lens, Perspective and the fused Lens/Perspective/straightened crop, each in byte and linear domains) for the 0.025–2 MP study. Native Lens timing, memory and GPU residency remain incomplete; no resample ratio is evidence for this pass.

On the RAW linear path the last segment's pass pulls a resample's taps through the segment before it (`load_resampled`), so its gate asks the resample's own threshold over the output, as the byte path's `resample_frame` does, and counts the pulled segment's colour units and masks with the segment's own. A byte segment pass after a warp, which resamples nothing itself, asks the transform's threshold for its geometry. `parallel_break_even_per_pass` includes the case `resample: 10 degree crop, linear`: at a one-minute load of about 5 its pooled/serial p50 was 0.26 at 0.034 MP, 0.20 at 0.069, 0.14 at 0.139, 0.12 at 0.347, 0.11 at 0.694 and 0.10 from 1.04 MP, the byte crop in the same run 0.42 at 0.034 MP down to 0.11 at 2.78 MP, so pooling wins at every size and the 0.1 MP threshold stands. A 10° crop of a RAW-linear stage with a 0.44 MP output takes 2.3 and 3.5 ms, and under a full Basic layer 6.2 and 7.5 ms (p50, two runs at a load of 5.5 to 11). Serial and pooled write the same bytes, and the test asserts that each case pooled through its own gate (`a_pass_between_the_shared_and_its_own_threshold_pools_to_the_serial_bytes`).

`editor-performance --samples 30` measures proxy renders at a 1280 × 800 display bound, where a 3:2 photograph's whole proxy is 1200 × 800 (0.96 MP) and the crop stack's proxy is 2078 × 1386 rendering a 1280 × 719 output: a full Basic layer and a full-strength Presence layer (texture, clarity and dehaze at 100) on each. Release `--locked`, in-process on the native Apple M4 Pro, holding the host-wide timing lock, two runs per size. Each cell is the p50 or p95 of each run in order.

| Proxy render at 1280 × 800 (ms) | p50 | p95 |
| --- | --- | --- |
| 24 MP upright, full Basic | 4.37 · 4.79 | 4.55 · 6.27 |
| 24 MP upright, Presence | 13.81 · 15.10 | 20.45 · 20.28 |
| 24 MP crop stack, full Basic | 7.55 · 7.51 | 9.41 · 7.99 |
| 24 MP crop stack, Presence | 50.33 · 52.04 | 56.65 · 60.30 |
| 60 MP upright, full Basic | 4.80 · 7.69 | 5.34 · 11.83 |
| 60 MP upright, Presence | 15.48 · 20.51 | 18.37 · 43.44 |
| 60 MP crop stack, full Basic | 7.82 · 7.89 | 8.42 · 32.70 |
| 60 MP crop stack, Presence | 58.56 · 71.05 | 65.98 · 890.09 |

The 60 MP source (10000 × 6000) fits the bound in 1280 × 768 (0.98 MP) and its crop stack's proxy is 2310 × 1386, rendering 1280 × 720. The 24 MP runs started at a one-minute load of 12.6 and 14.2, the 60 MP runs at 29.9 and 27.3, a loaded host; the second 60 MP run is the loaded one (its proxy builds took 26 to 28 ms against 7 to 11 ms otherwise), so its tails are the host's. Below one megapixel the upright Presence render's tiles run on the pool, as does the crop stack's 1280 × 719 resample; the crop stack's colour pass on the 2.9 MP proxy stage and the upright full Basic render pool through the heavy-colour gate. At the 2880 × 1800 bound every pass is above the shared one-megapixel threshold too.

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
| Idle CPU, 24 MP with the full Basic layer, 30 s after settling | 1.38% of one core (one sample) |
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

The histogram plot's canvas program holds an `iced::widget::canvas::Cache` in its own persistent
state, keyed by a version the view derives from the render identity and the `stale` flag
(`crates/luxforge-ui/src/widgets/histogram.rs`,
`crates/luxforge-app/src/view/tools_panel.rs::plot_version`), so a redraw with unchanged bins reuses
the tessellated polygons rather than rebuilding three 256-point fills. The idle figure is
`editor-latency --source fixtures/generated/24mp.jpg --idle --samples 3 --output
artifacts/idle-after-basic`; the `histogram` smoke scenario (`artifacts/idle-histogram`) confirms
the cached plot renders and updates correctly.

### Editor process measurements

`measure`, five app-cold launches per workload plus one repeated 60 MP run, on the host above.
Launch to observed frame is an upper bound: it includes the temporary
background bundle, the executable copy and the harness's capture readback, not scanout.

| Measurement | Result |
| --- | --- |
| Launch to observed frame, empty (median / p95) | 671.2 / 751.7 ms |
| Launch to observed frame, 24 MP (median / p95) | 734.7 / 742.5 ms |
| Launch to observed frame, 60 MP (median / p95) | 808.8 / 857.4 ms |
| Sampled peak RSS (empty / 24 / 60 MP, median) | 133.2 / 466.0 / 975.0 MiB |
| Sampled peak RSS after sixteen 60 MP loads | 1316.4 MiB |
| Open request to captured frame (24 / 60 MP, median) | 222.2 / 314.3 ms |
| … of which GPU upload (24 / 60 MP, median) | 21.4 / 52.3 ms |
| Open to full-resolution CPU raster (24 / 60 MP, median) | 178.1 / 240.7 ms |

### Instant previews: proxy phase, hop rule and the surface primitive

Native Apple M4 Pro, macOS 26.5.2, Metal, a 2880 × 1800 physical window at 2× scale, release
builds, background hidden-window launches, warm filesystem cache, one-minute load averages between
4 and 7.5 on a shared host. These rows measure the CPU proxy and exact phases, which a drag takes
only on a machine without a GPU ([GPU previews qualified on the M4](#gpu-previews-qualified-on-the-m4)).
Presented here means the update in which the frame became the photo surface's source; it is drawn by the redraw that update requests, the next
frame, and it is still not scanout. The exposure gesture is a drained drag; a full
Basic layer means `--basic`, which commits all ten fields non-neutral first so every frame runs
every colour unit of the module.

| Drained drag, input to presented frame (p50 / p95 ms, 30 samples) | Fit |
| --- | --- |
| 24 MP, exposure only | 9.5 / 29.0 |
| 24 MP, full Basic layer | 18.1 / 24.8 |
| 60 MP, exposure only | 10.1 / 19.1 |
| 60 MP, full Basic layer | 17.2 / 28.9 |
| 24 MP, 7° crop-fit and full Basic layer | 16.1 / 28.7 |

The figures do not depend on the source size, because every frame in a drag is the proxy phase:
a 1716 × 1144 render of the whole recipe against the cached display-bounded proxy of the source
(the photo area of this window at 2×), presented through the surface primitive with no allocation
round trip.

| Wild drag, 120 inputs per second for 3 s alternating direction (`--mode burst`) | Presented fps | Staleness p50 / p95 ms | Largest gap ms |
| --- | --- | --- | --- |
| 24 MP, exposure only | 53.7 | 18.1 / 32.9 | 41.8 |
| 24 MP, full Basic layer | 41.7 | 32.9 / 34.1 | 36.1 |
| 60 MP, full Basic layer | 42.5 | 32.7 / 34.1 | 32.4 |

Every one of the 360 scripted values reaches the owner as its own `draft.set` (the gesture's
round trip is synchronous and takes 0.18 / 0.19 ms), the queue keeps one proxy job active and one
pending, and every superseded exact phase is cancelled (160, 124 and 124 of them in the three runs).

| Settled exact histogram after the last input (p50 / p95 ms) | Figure |
| --- | --- |
| 24 MP, exposure only, commit mode, 30 samples | 60.1 / 70.3 |
| 24 MP, exposure only, drag mode, 2 commits | 54.3 / 54.3 |
| 24 MP, full Basic layer, 2 commits | 153.8 / 158.6 |
| 60 MP, full Basic layer, 2 commits | 356.4 / 367.0 |

The settled histogram is the exact phase of the committed frame: the full-resolution render with
every unit, then the reduction. It is not on the input path, so a drag does not wait for it; the
60 MP full-Basic figure misses the 200 ms threshold set for 24 MP and is recorded here because the
design asks for the tails.

The per-leg timings the `slider_draft_preview` event records: the owner's `draft.set` and
preview-job work take under 0.2 ms, the desktop's update, model derivation and view under 0.15 ms
together. A message handed back into the update loop through the runtime arrives about 7.9 ms
later, one frame of the 120 Hz display, because a redraw is always in flight during a drag: with
the round trip as a task and the frame through an image allocation, a 24 MP drag measured
38.6 / 63.0 ms, and with the round trip synchronous, 30.2 / 58.7, which is why the gesture's round
trip is synchronous and the frame goes through the surface primitive.

#### A gesture's press to its first frame

A press opens the gesture's draft with `draft.begin`, and its first `draft.set` cannot go before
that answers. `draft.begin`, `draft.reapply` and `draft.cancel` run synchronously on the desktop
thread, as `draft.set` does, so the press's first `draft.set` goes in the press's own update.
Measured with `editor-latency --mode drag --samples 5` on the 24 MP JPEG at Fit, exposure only, and
the same with `--mask` (the slider bound to a linear gradient mask), on the host and window above
(Apple M4 Pro, macOS 26.5.2, Metal, 2880 × 1800 at 2×), release builds, background launches, warm
cache. `xtask` reports two rows from the gesture's `slider_draft_begin` (logged in the update that
opens the draft): `press_to_first_draft_set` and `press_to_first_presented_frame`, the frame of
that first set's preview job. A drag has one measured press per launch (a second, the release
step's, adds a `press_to_first_draft_set` sample), so each row below is 30 or 16 launches, in two
blocks of 15 (8 masked) on a heavily shared host, one-minute load 21–34.

| 24 MP drag, p50 / p95 ms | Block 1 | Block 2 |
| --- | --- | --- |
| Press to first `draft.set`, exposure (30 per block, two presses a launch) | 0.05 / 0.06 | 0.05 / 0.08 |
| Press to first presented frame, exposure (15 per block) | 8.07 / 9.41 | 7.77 / 16.68 |
| Press to first presented frame, masked (8 per block) | 9.71 / 25.53 | 9.34 / 9.78 |
| Later inputs, input to presented frame, exposure (75 per block) | 8.73 / 10.51 | 8.60 / 35.09 |

Pooled over both blocks, a press reaches its first frame in 7.83 / 9.41 ms (30 launches), and in
9.42 / 25.53 ms with the mask (16). The press costs what every later input costs, and its first
`draft.set` leaves 0.04–0.17 ms after the press. The p95 tails are the shared host: single
launches, not the build, carry them.

The mask shape and crop gestures take the same path and have no `editor-latency` mode of their own.
Their press to first `draft.set`, read from the `mask_draft_begin`/`crop_draft_started` events of
one functional run each of the `mask-panel`, `mask-brush` and `crop-draft` smokes on the small
fixture, is 0.04–0.07 ms for five mask shape presses and 0.004 ms for the crop's two starts. Those
are single functional runs, not distributions.

#### A brush stroke's press

A brush in hand holds no core draft: each stroke's `draft.begin` runs at its press, synchronously,
immediately followed in the same update by the first `draft.set` carrying the press's position.
`editor-latency --mode paint` reports the same two press rows drag mode does, from the stroke's
first `mask_stroke_position` (logged in the update that hands the press to the desktop):
`press_to_first_draft_set` and `press_to_first_presented_frame`, the first of the stroke's own
frames presented. Measured on the 24 MP JPEG at Fit with the default 30-position stroke at 24 ms,
on the host and window above (Apple M4 Pro, macOS 26.5.2, Metal, 2880 × 1800 at 2×), release
builds, background launches, warm cache. One stroke is one press, so each block is 15 launches and
15 presses, on a heavily shared host, with the one-minute load each launch recorded.

| 24 MP paint, p50 / p95 ms | Block 1 (load 25.6–34.0) | Block 2 (18.5–26.9) |
| --- | --- | --- |
| Press to first `draft.set` (15 per block) | 0.06 / 0.09 | 0.06 / 0.10 |
| Press to first presented frame (15 per block) | 10.58 / 15.65 | 10.66 / 58.73 |
| Later positions, input to presented frame (398–435 per block) | 8.28 / 32.09 | 8.37 / 28.91 |

Pooled over both blocks (30 presses), the press reaches its first `draft.set` in 0.06 / 0.09 ms
and its first frame in 10.60 / 18.70 ms. Opening the draft at the press costs the synchronous
`draft.begin`, about 0.05 ms at p50. The p95 tails are single launches on the shared host (block
2's 58.7 ms is one launch), not the build.

| Core diagnostic, 24 MP crop stack (p50 / p95 ms, 10 samples) | Full resolution | Proxy for 2880 × 1800 |
| --- | --- | --- |
| Same stack without colour | 29.1 / 32.9 | 16.9 / 19.0 |
| One +1 EV Basic layer | 35.8 / 37.5 | 21.1 / 23.0 |
| Full Basic layer | 77.3 / 79.7 | 46.9 / 48.0 |
| Proxy build (a cache miss: once per source, bounds and window size) | — | 21.2 / 25.7 |

The proxy source for this stack is the whole 4677 × 3118 proxy stage, larger than the
2879 × 1618 output it produces, because the 16:9 crop discards most of the rotated stage; the
colour pass covers only the band of rows the crop reads. The full-Basic proxy render is the
largest per-input cost on this path.

#### A crop draft's input stage at Fit

The stage at Fit is the GPU's picture at rest of the prefix, with the reference's exact frame reduced
to the view where the GPU cannot draw it
([instant previews](../design/instant-preview.md#a-crop-drafts-input-stage)).

Aliasing at Fit is settled on a generated 6000 × 4000 zone plate (a radial chirp reaching 0.5
cycles per pixel in the corners, JPEG quality 95 without chroma subsampling): the crop draft opened
at Fit through `editor-latency --mode crop-start --samples 1`, and the captured stage compared,
over the 77% of it whose source frequency lies beyond the display's Nyquist limit, with a
linear-light box downscale computed independently. The exact stage drawn by the surface's bilinear
sampler without mip levels aliases: full-contrast replicas of the centre rings across the frame, a
standard deviation of 63.6 codes there and a mean of 145.1, 16 codes darker than the true average.
The display-size stage does not: it shows only the faint residual rings of a box filter, 15.8 codes
against the reference's own 10.9, with a mean of 160.3 against 160.8. A percentage zoom draws the
exact stage at or above its size, where the sampler magnifies and cannot alias.

RAW, one functional trial per camera through `raw-editor` (the 13-step journey of the RAW tables
below, so these are single launches and not distributions): request to display of the exposure
step is 10.5 ms on the Z6, 14.6 ms on the X100VI and 15.0 ms on the Air 2S; rotate, crop-fit and
undo present in 18 to 46 ms. A committed temperature, tint, gain or neutral pick redevelops the
mosaic on the source worker before its exact frame exists; that is the release cost listed in the
[performance rules](../engineering/performance-rules.md#known-remaining-costs), measured in
[startup and RAW throughput](#startup-and-raw-throughput).

#### A minified After side at Fit

Where the GPU cannot draw its picture, Before/After keeps the exact raster it displays as its After
side, even at Fit, so the photo surface draws a full-resolution texture at well under half its
size, the case the crop draft's zone-plate measurement shows aliasing for through a bilinear
sampler with no mip levels. Such a texture has linear-light mip levels
([gpu previews](../design/gpu-preview.md#mipmapped-minification),
[before and after](../design/before-after.md#after-at-fit)).

The judgement is the `compare-zone-plate` smoke scenario, whose statistics
(`xtask/src/zone_plate.rs`) are the ones named above: the capture's mean and standard deviation of
luma over the pixels of the drawn photograph whose source frequency lies beyond the display's
Nyquist limit, beside those of an independent area-weighted box average of the decoded source's
linear light, re-encoded. The fixture is a generated 6000 × 4000 zone plate (`zone-plate.jpg` from
`cargo xtask generate-fixtures`): `round(127.5 + 127.5 cos(pi k r^2))` with `k` such that the
frequency reaches 0.5 cycles per pixel at the corners, JPEG quality 95 without chroma subsampling.
Its region beyond Nyquist is 86.1% of the photograph at Fit (1,689,852 of 1716 × 1144 pixels),
against the 77% of the crop-stage measurement's plate, so the reference's own standard deviation
there is 18.1 codes where that plate's is 10.9, and the figures below are comparable with each
other but not number for number with those.

Scope: native Apple M4 Pro, macOS 26.5.2, Metal; release builds launched hidden in the background
bundle at 1440 × 900 logical, 2× (2880 × 1800 physical); the fixture opened at Fit, left 4 s for its
exact render to land, then compared with `\` and After alone revealed (`Position(0.0)`), judged on the
captured frame. The host was shared (one-minute load average 12 to 19).

| 24 MP zone plate at Fit, 1716 × 1144 | Figure |
| --- | --- |
| Display proxy before the comparison (control): standard deviation, mean | 18.06, 159.98 |
| Reference box reduction: standard deviation, mean | 18.05, 159.99 |
| After alone: standard deviation, mean | 12.38, 160.55 |
| After alone: standard deviation against the reference's, mean against its | 0.69 times, 0.57 above |
| Resident photo-slot bytes with the comparison open | 144,776,244 |
| Mip levels resident, mip chains generated | 31,999,028 bytes, 1 |

Without mip levels the same capture reads 64.03 and 144.67: full-contrast replica rings across the
frame and a mean 15 codes below the true average. With them the frame shows only faint residual
rings; its standard deviation is below the reference's because the box-filtered levels, read
trilinearly, are softer than the single box reduction. The After texture is 6000 × 4000 with its 13
levels, 96,000,000 bytes of base and 31,999,028 of levels, which the resident bytes include. The
display-size photograph holds no levels, before the comparison and after it ends.

The generated 60 MP JPEG (10000 × 6000) is held in two tiles by the device's 8192 pixel textures and
cannot have levels, so there After draws the display reduction at Fit (`compare-tiled`): After alone
equals the display-size photograph drawn before the comparison to the code (largest channel
difference 0), and the resident photo-slot bytes with the comparison open are 33,554,432, since the
240 MB tiled texture is not drawn at Fit.

The `workspace` scenario, which holds the Before/After checks for crop and orientation alignment,
endpoints, holds, zoom alignment and unchanged divider uploads, passes with 62 pixel claims held and
none failed. Not measured: the GPU time of a chain's passes and their encoding time on the UI
thread, a Fit drag or window resize while comparing, and any native run off this host; the
headless tests run on the same adapter.

#### The proxy build's memory and time

The proxy's box downscale runs in bands of output rows, each worker holding one intermediate of
about 1 MiB ([instant previews](../design/instant-preview.md#the-cpu-proxy-a-session-without-a-gpu)).
Measured with the ignored `measure_the_proxy_build_on_photo_sized_sources` test in
`cpu_proxy/tests.rs`: one process per run decodes the generated JPEG, then builds its proxy for the
owner's 2880 × 1800 display bounds 30 times, and `/usr/bin/time -l` reports the process's maximum
resident set size. The same run with `LUXFORGE_PROXY_BUILDS=0` decodes and builds nothing, which
is the floor the build's peak is read against. Native Apple M4 Pro (14 cores), macOS 26.5.2, Rust
1.94.0, release test binaries, warm filesystem cache, four runs, one-minute load average 11.6–13.7
with other sessions building.

| Maximum resident set size (MiB) | 24 MP, 6000 × 4000 → 2700 × 1800 | 60 MP, 10000 × 6000 → 2880 × 1728 |
| --- | --- | --- |
| Decode only | 102.2–102.4 | 240.8–240.9 |
| Decode and build, four runs | 148.1–155.4 | 290.8–294.7 |
| The build above the decode floor | 45.9–53.2 | 49.9–53.8 |

The build's peak is the proxy plus the pool workers' band intermediates and the allocator's
retention of them, and it does not grow with the source. Peak footprint (`/usr/bin/time -l`'s other
figure) is 141.2 MiB at 24 MP and 283.0 MiB at 60 MP.

| Build time, 30 builds per run (ms) | Four runs |
| --- | --- |
| 24 MP, first build of the process | 7.9, 7.7, 12.0, 13.9 |
| 24 MP, p50 | 7.4, 7.1, 8.3, 14.5 |
| 60 MP, first build of the process | 14.2, 13.5, 20.2, 13.2 |
| 60 MP, p50 | 13.0, 15.3, 16.4, 13.2 |

Each cell lists the runs in run order; the fourth 24 MP run went through a load burst (p95
43.5 ms).

#### The workspace derivation per message

The desktop derives every region of the workspace — title bar, state panel, canvas, Masks panel,
tools panel, histogram, status bar, palette and the Performance section — after every message, with
no per-region keys ([develop workspace](../design/develop-workspace.md#message-flow)). What
that costs is the `detail.loop.last_rederive_ms` of `slider_draft_preview`: the `rederive` span of
the update before that event's own, with a capability section's second derivation and the visible
curves' sample requests inside it; `last_view_ms` is the `view()` of the same update. One sample per
drafted input. Native Apple M4 Pro (14 cores), macOS 26.5.2 (25F84), Metal, a 2880 × 1800 window at
2×, background hidden-window launches of release builds, `--locked`, warm filesystem cache, the
generated 24 MP and 60 MP JPEGs at Fit, Basic's exposure slider, and with `--mask` the same slider
bound to a linear gradient. The comparison is a keyed build of the same tree (region keys,
`Tracked` stamps and per-section digests), the alternative the derivation was measured against.
`editor-latency --mode drag` (30 inputs, 62 `slider_draft_preview` events a launch) and
`--mode burst` (360 events a launch), each workload run keyed, current, current, keyed back to
back, so every figure below pools two launches: 124 samples a drag cell and 720 a burst cell. The
host was shared with other agents building throughout; the one-minute load is given. Nearest-rank
p50 / p95 in ms of `last_rederive_ms`.

| Every region derived against keyed | Drag, load 14–18: keyed · every region | Burst, load 21–37: keyed · every region |
| --- | --- | --- |
| 24 MP | 0.052 / 0.192 · 0.116 / 0.270 | 0.008 / 0.029 · 0.109 / 0.580 |
| 60 MP | 0.021 / 0.120 · 0.086 / 0.143 | 0.006 / 0.026 · 0.063 / 0.189 |
| 24 MP masked | 0.028 / 0.368 · 0.095 / 0.219 | 0.007 / 0.026 · 0.048 / 0.157 |

Deriving every region costs about 0.05 to 0.14 ms at p50 per message, against 0.003 to 0.05 ms with
the keys. The p95 follows the host rather than the build: in drags the keyed build's own p95
reached 0.24 to 0.37 ms, and a launch whose derivation p95 is high has its `view()` inflated with
it. The quietest burst launches hold p95 at 0.126 ms (60 MP) and 0.082 ms (masked); the busiest
reach 0.68 ms. Input to presented frame is the same with and without the keys: p50 7.7 to 9.5 ms,
one frame of the 120 Hz display.

Where the time goes, from a diagnostic build that timed each region (not kept; drags and a burst of
one launch each, load 28–31): outside a mask the tools panel takes 64 to 67% and dropping its
previous model 7 to 11% more, and the Masks panel 16 to 21%; in the masked drag the tools panel
takes 44% (and 8% to drop) and the Masks panel 43%; the canvas 1 to 7%; every other region 3% or
less, the palette 0.1%. The tools panel's time is spread over its sections by the controls they
build: in a burst the Presets section 33%, the colour mixer 32% and Basic 20%; in the masked drag
Basic 62% and the mixer 34%. The tools panel reads almost every input the editor holds, so no one
value keys it without the input tracking the keys were built on, and during a drag it is derived
for every input anyway. The decision this informs is recorded in
[decisions](../decisions.md#post-consolidation-review).

### The photo surface's bound and retirement

The photo bound is a current and a retiring allocation of at most 512 MiB each within 1 GiB, for
photo textures only. It is not a total editor or GPU product budget. Crop GPU textures and tiles,
overlays and backend staging sit outside that photo accounting and are not fully measured or
bounded; native accounting and bounds remain required before any total-memory guarantee.

The two ignored Metal retirement diagnostics: an empty render-thread submit waits 5.125 µs with one
retirement in flight during a 63.389 ms GPU workload, and fifteen smaller retired workloads produce
submit waits of roughly 2.167–25.667 µs. Each trial asserts a real retiring texture. These are
timing diagnostics on a shared host, not a CI timing threshold.

### Provisional targets: measured

The GPU editor's figures for the slider, settled histogram, burst, memory, idle, launch and
uncached-open targets are in [the timing tier](#the-timing-tier). The rows below are the targets it
does not measure. A miss is a finding for the owner's review, not a blocker.

| Provisional target | Measured | Verdict |
| --- | --- | --- |
| Instant preview: drained drag p95 ≤ 33 ms at Fit, 24 and 60 MP, full Basic layer, with and without a 7° crop | 24.8, 28.9 and 28.7 ms p95 on the CPU proxy (30 samples each) | **Pass** |
| Scratch aggregate at most 64 MiB | 13.46 MiB high-water at 24 MP, 12.82 MiB at 60 MP | **Pass** |
| Geometry input to presented preview p95 < 50 ms once the source preview is ready | not measured for geometry | Open |
| A masked drag costs a person no more than an unmasked one | 24 MP drained drag p50 16.77 / 17.13 ms masked against 16.93 unmasked, 60 MP 17.06 / 17.87 against 17.00 / 17.48, all with a full Basic layer, in both orders at load 2.3–8.1; masked Clarity at 24 MP 17.18 / 17.10 against 17.00 / 17.10 unmasked | **Pass**: every masked run is within 0.4 ms of its unmasked pair at the median, which is inside the spread between two runs of the same condition |

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
`artifacts/final-latency-60-drag`, `artifacts/final-latency-60-commit` and `artifacts/final-measure`
retain every sample, the correlated state and the source and binary hashes; they are local evidence
and are not repository assets.

Current macOS `measure` and `editor-latency` runs use background-only bundles to preserve desktop focus, and the editor they launch creates its window invisible. Launch-to-frame timings include copying the executable and creating its temporary bundle; they are background renderer measurements, not foreground activation measurements, and they exclude the cost of placing and compositing a visible window. Nothing in these runs is scanned out, so the presentation figures cover the editor's path to a renderer texture and not what reaching a display would add. Rendering, readback and the state each frame is correlated against are the same as a visible window's: the window owns the same Metal surface either way. Reports identify the launch mode.

## Current RAW and JPEG measurements

Native Apple M4 Pro, 48 GiB RAM, macOS 26.5.2 (25F84), Metal, 2× scale and a
2880×1800 physical window; files on the internal 2 TB APFS SSD. Release builds,
background-only launches, warm filesystem cache without an OS cache purge.
The native adapter configuration is recorded in the [backend selection](../research/raw-backend-selection.md).

Thirty complete trials per owner camera passed. Each trial has an isolated catalog,
13 edit/history/view steps, per-step captures and a second-process reopen. The
owner's Z6 is 14-bit lossless NEF; the X100VI is 14-bit uncompressed RAF. All four
public minimum modes also pass one complete trial each; those are functional
checks, not latency distributions. Original hashes, displayed entry/snapshot,
controls, geometry and reopened photo samples are checked together.

Times below are nearest-rank p50 / p95 in milliseconds. Open timings stop at the
CPU raster; launch-wrapper time and fine-grained stage attribution are not included.
The editing, history and view steps are measured in
[instant previews](#a-crop-drafts-input-stage-at-fit),
[second development](#second-development) and
[startup and RAW throughput](#startup-and-raw-throughput).

| Measurement (ms, p50 / p95) | Z6 | X100VI |
| --- | --- | --- |
| Initial open → full-resolution CPU raster | 840.0 / 868.2 | 1828.6 / 1887.0 |
| Edited catalog reopen → CPU raster | 1170.2 / 1222.2 | 3267.8 / 3383.2 |

Sampled first-process peak RSS (roughly 50 ms sampling) is 1323 / 1339 MiB p50 / p95
for Z6 and 1975 / 1992 MiB for Fuji. Fuji trial 25 has an unexplained 2436 MiB peak
and a 1038 ms crop update (1007 ms source-to-raster); both tails are retained. Its
pixel, state and reopen checks pass. The capture-heavy workflow cannot isolate
CPU heap, native allocator retention, GPU resources or readback buffers. Separate
screenshot-free live API runs peak at 1583–1647 MiB; the 24-edit run grows only
1.25 MiB after edit three. This does not establish a whole-process bound or prove
absence of leaks. GPU allocations are not measured separately.

Initial development p95 meets its provisional investigation target on these files;
Fuji memory exceeds the 1536 MiB target. The next resource work is to attribute
the unexplained tail and native/GPU/readback lifetimes, then evaluate bounded
Fit/detail rendering while preserving full-resolution 100% inspection. Budgets
remain provisional; full idle-CPU, cancellation and per-stage measurements remain
open.

The RAW rows were taken with a 30-trial RAW editor timing tool whose journey is the `raw-editor`
smoke scenario ([development](../engineering/development.md#authentic-raw-evidence)), which records
one functional run's request-to-display times per source; a RAW distribution is 30 runs of it, not
one. Local reports retain every trial, percentile input, source/binary hash and failure;
private photographs and captures are not repository assets. These observations
qualify the recorded files and host, not other camera modes or platforms.

## Air 2S DNG measurements

The supplied FC3411 uncompressed DNG passes 30 complete background editor trials,
each with the same 13-step editing/history/view journey and second-process reopen.
Original hashes, correction provenance, geometry, displayed state and sampled
photo pixels agree. These are native M4 Pro measurements under the configuration
above, with a warm filesystem on a shared host; host isolation is not claimed.

| Measurement (ms) | p50 | p95 | Maximum |
| --- | ---: | ---: | ---: |
| Initial open → full-resolution CPU raster | 1692.6 | 1811.9 | 1812.3 |
| Edited catalog reopen → CPU raster | 3092.7 | 3412.0 | 3418.2 |

Sampled first-process peak RSS is 1245.3 / 1250.5 MiB p50 / p95,
with a 1258.8 MiB maximum; reopened processes measure
718.6 / 731.3 MiB with a 731.4 MiB maximum. Sampling is
roughly every 50 ms. The optical pass adds one reusable 76.15 MiB active-plane
scratch after native demosaic scratch is released; the allocation ledger is in the
[Air 2S design](../design/air2s-dng.md#allocation-and-performance-review).
The captures, GPU resources and allocator retention are included in observed
process memory, not separated. These values do not establish a process-wide bound,
GPU-memory budget or native Windows/Linux result. The Fuji resource
qualification above remains open.

Reproduce as above. Local reports under `artifacts/air2s-editor-30-01/` retain every sample,
source hash and correlated state; private originals and captures are excluded from source
control. Single-trial Nikon/Fujifilm editor regressions also pass, separately from the timing
distributions. The host package passes one complete DNG journey; its bundled native notices are
present and runtime linkage uses no system RAW library. This is an unsigned macOS development
package, not a license audit or platform qualification.

## UI components qualification

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2 (25F84), Metal, internal APFS SSD,
release `--locked`, warm filesystem cache. Desktop captures are 2880 × 1800 at 2×;
the gallery is 2880 × 2000. All launches use the background bundle. Measurements run
sequentially with compilation and tests stopped; the shared host is not claimed to be quiescent.

The 6000 × 4000 JPEG drag workload uses 30 drained inputs, one release and a coalesced burst.
Input-to-frame ends at the renderer's `Uploaded` callback, not display scanout. The curve is
visible and its module supplies the sampled polyline; its proof effect preserves photo pixels,
so this measures the gesture, draft, preview and upload path, not a Tone Curve processor.

| Input-to-frame (ms) | p50 | p95 | Maximum |
| --- | --- | --- | --- |
| Basic slider | 70.66 | 87.75 | 91.82 |
| Curve point drag | 57.09 | 74.76 | 108.17 |

Both p95s miss the [provisional slider budget](#provisional-budgets) (p95 < 16 ms; a miss at or above 32 ms).

The curve timing uses exactly representable fractions and keeps strict request/frame checks.
The complete samples, hashes, captures, state and logs are indexed by
`artifacts/ui-components-verification/summary.json`, including `ui-components-after-slider` and
`ui-components-after-curve-final`. Gallery and controls smoke cover 63 states on 10 pages and
22 interactions respectively. Existing private RAW, manual visual/fixture-generation and explicit
measurement tests remain skipped; this qualification makes no native Windows/Linux GPU claim.

## Presence, colour mixer and vignette qualification

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, release `--locked`, warm source cache, background bundle launches for every desktop figure; core figures are in-memory synthetic frames rendered by the core alone, warm, with the estimate store warm where a global estimate exists. Every desktop figure ends at the renderer's `Uploaded` callback, not display scanout. All three modules run the same full-resolution draft path Basic runs: nothing approximate, no extra cache and no timer is added to reach any figure, and each miss below is a finding for the owner's review.

### Core cost of the units

The three units together cost more than the sum of the singles, and that is the halo, not a hot loop: with a summed halo of 448 px a 512 px tile reads a 1408 px input region, dehaze fills 1194 px and texture 1166 px of it to deliver 512 px, so over the stage dehaze computes 5.2 times its pixels and texture 4.9 times, and the 101 MiB working set holds a batch to two tiles. Each of those tiles runs its own passes on the pool (`Parallelism::Pool`, see the [architecture](../design/architecture.md#stage-boundaries)). Larger tiles repeat less of the halo, measured below; tiling each unit separately would need an intermediate float frame between units, 687 MiB at 60 MP, over the JPEG frame limit. The current `presence_timing` figures are in [CPU and memory efficiency](#cpu-and-memory-efficiency-measured-on-the-m4). The vignette's unit alone, single-threaded over 6000 × 4000: 57 ms at amount −50, 505 ms at +50 (the positive branch encodes and decodes each channel), 164 ms at roundness −100.

#### Lazy vignette falloff tables

`Vignette` builds its two falloff tables — `width + height` entries, one `powf` each on the superellipse branch a negative roundness selects — lazily, in a `OnceLock` per table, on the first call that actually reads a pixel (`apply_row`). `modules/registry/compile.rs`'s stage-only callers (`editor/plan.rs`'s `with_stage_context`: every plan, `draft.set`, a composite step and a query) need only `describe` or `is_finite` and never read a pixel, so they never pay for the tables; `is_finite` checks the shape's own coefficients, which is exactly as strong because every table entry is a pure function of them.

Native Apple M4 Pro, release `--locked`: 20,000 calls of `Vignette::new` plus one `describe` and one `is_finite` — the owner-thread cost those stage-only callers pay per compile — at 60 MP (9504×6336) with roundness −100, so the superellipse branch's tables are the ones measured, in two reversed runs (`compile_cost_per_call_at_60_megapixels`):

| | ns/call (two reversed runs) |
| --- | --- |
| Lazy, never built (`describe`/`is_finite` only) | 138.1, 136.6 |
| Both tables built immediately after construction | 103378.6, 105421.5 |

A stage-only compile call costs under a microsecond; building the tables costs roughly 0.10–0.11 ms, which at the two to three compiles per slider tick `docs/engineering/performance-rules.md`'s vignette row estimates would be on the order of 0.2–0.3 ms of owner-thread time per tick whenever the stack holds a superellipse vignette, against a `draft.set` round-trip p50 of about 0.2–0.3 ms. Renders and `describe`/`is_finite` output are unaffected: the oracle, mirror/flip-symmetry and zero-mask tests pass, and `describe_and_is_finite_never_build_the_tables_and_a_pixel_read_builds_each_once` proves neither call builds either table and that a pixel read builds each exactly once.

#### Presence exact renders on the generated JPEGs

The core render of the generated 24 MP and 60 MP JPEGs with one Presence layer at +100 in each field it names, warm source and estimates, the 256 MiB target, timed by an uncommitted release probe that calls `render` as `presence_timing` does and reads the process's CPU time around each render; `presence_timing` is the committed measurement to repeat. Each tile's passes ran serially and on the pool, four runs alternately, 5 samples per stack and run, at a one-minute load of 5 to 14; every rendered frame had the same SHA-256 either way for all five stacks at both sizes. p50 of each run:

| Stack | Serial passes | Pooled passes | Concurrency |
| --- | --- | --- | --- |
| 60 MP, all three | 12758 · 10708 ms, 182 · 192% | 3868 · 3854 ms, 1165 · 1191% | 2 |
| 60 MP, Texture and Clarity | 5605 · 5613 ms, 282 · 280% | 2923 · 2889 ms, 1139 · 1175% | 3 |
| 60 MP, Clarity | 576 · 600 ms | 609 · 636 ms | 8 |
| 24 MP, all three | 1358 · 1414 ms, 371 · 369% | 944 · 929 ms, 1137 · 1166% | 4 |
| 24 MP, Texture and Clarity | 879 · 951 ms | 695 · 682 ms | 5 |

With serial passes the process uses at most one core per tile in flight whatever the host's load (182 to 192% for two tiles); the batches themselves are 93% efficient and the one Dehaze reduction takes 6 to 34 ms, so neither is the cause. Where a batch is as wide as the pool (Texture or Dehaze alone) its tiles keep their passes serial, and the two strategies agree within 5% in both orders at a target wide enough to make every batch fill the pool. The Clarity row, whose eight-tile batches spread over the pool, moved by about 5% in either direction across runs. The cost is CPU time: the two pooled runs used 697 and 700 CPU-seconds against 370 and 360 serially over the same renders, because fourteen workers on two tiles' memory-bound passes each run slower and the pool spins between short passes.

Two alternatives were measured and not taken. Raising the spatial target to 4 GiB lets fourteen tiles run at once: all three at 60 MP took 3326 ms at 834% (10 samples, load 10 to 22) but the budget peaked at 1415 MiB, against 202 MiB. Counting only the two plane buffers a tile holds at once would lower the all-three working set from 101.1 to 82.5 MiB, three tiles instead of two. Tiles of 1024 px with pooled passes took 2093 ms for all three at 60 MP and 1616 ms for Texture and Clarity (5 samples, load 8 to 12) with the same bytes; an operation whose summed halo passes 128 px runs in them ([tile size by summed halo](#tile-size-by-summed-halo)).

#### Tiles written back by row

A spatial tile's output is produced in the frame's own row layout on the worker that ran the tile, the byte frame's RGBA rows quantized and with the input's alpha, and written back with one `copy_from_slice` per row (three per row on the linear planes), and each batch slot reuses its unit scratch rather than allocating and zeroing it per tile. The release probe for the figures in [tile size by summed halo](#tile-size-by-summed-halo) renders through `luxforge_core`'s public `render` as `presence_timing` does: one Presence layer at +100 in the fields each row names, warm source and estimates, the 256 MiB target, and the process's CPU time around each render, on the generated JPEGs. A point sample through the layer evaluates its tile with an empty scratch slot.

#### Tile size by summed halo

`presence_tile_sizes` (`cargo test --release --locked -p luxforge-core --lib presence_tile_sizes -- --ignored --nocapture`, with `LUXFORGE_PRESENCE_SOURCES` naming the sources), native Apple M4 Pro, release `--locked`, holding the host-wide timing lock on a host shared with other sessions: the one-minute load was 11 to 36 around the runs. Each row renders one Presence layer at +100 in the fields it names, with a warm source and warm estimates, in 512 px and in 1024 px tiles, alternating which size goes first. A figure is the p50 of 5 renders per size (7 on the synthetic stages and in each row's second figure), with the process's CPU time over the render as a percentage of one core, and the p50 of 9 interior point samples at each size, each equal to the rendered byte. Every row's frame had the same SHA-256 in both sizes. The synthetic stages are the test's textured frames, sized to put halos between the fixtures'. The Tile column is the side `render::limits::spatial_tile` chooses.

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

The rule's effect on the whole render, from the release probe of [tiles written back by row](#tiles-written-back-by-row), in 512 px tiles for every stack and with the rule: runs back to back and reversed (the rule, 512 px, 512 px, the rule) at a one-minute load of 12 to 20, 5 renders per stack and run, and the p50 of 9 interior point samples per run, each equal to the rendered byte. Each cell is the p50 of each run, paired in the order the runs went; every stack's frame has the same SHA-256 either way.

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

Three first-pass figures with the rule ran at about 300% CPU while another process held the cores (24 MP Texture and Clarity, 60 MP Texture and Dehaze); their second passes are the comparison. Every stack that runs in 1024 px tiles renders 22 to 58% faster at about the same share of the pool, so its CPU time falls in proportion: all three fields at 60 MP take 1.5 to 1.6 s against 3.0 to 3.5 s in 512 px tiles. Its point samples cost 1.9 to 3.8 times as much, 33 to 46 ms with Clarity alone at 24 MP and 168 to 173 ms with all three at 60 MP. Texture alone, Dehaze alone and Texture with Dehaze keep 512 px tiles.

### Desktop slider-to-presented-frame

The drags of all three modules are drawn on the GPU ([GPU previews qualified on the M4](#gpu-previews-qualified-on-the-m4)).

Rendered evidence is the `presence`, `mixer` and `vignette` smoke scenarios (15, 8 and 12 correlated frames at Fit and 100% with the module's own controls visible), and the field-patch conformance suite's checks per module through the JSON method table. A reviewer's render of the owner's 14 MP Sapa drone JPEG through the core alone (release, in memory: dehaze 65 ms, clarity 104 ms, texture 127 ms, all three at +50 672 ms) showed Dehaze +60 and +100 lifting the veil and deepening colour plausibly, Clarity +100 adding local contrast without visible halos at fit and at 100%, and Texture +100 sharpening fine detail with the expected crunch; it is a visual check, not a measurement. On a synthetic haze-free flat field Dehaze +100 drives the field toward black, because the dark-channel prior reads a uniform patch darker than the atmosphere as pure veil and the frozen `OMEGA_MAX = 1` removes all of it; the study records this and real photographs, whose windows contain dark pixels, do not show it.

## Colour grading, qualified and measured on the M4

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, release `--locked`, warm filesystem cache.
All 21 measurement commands ran serially behind the host timing lock, with no builds or tests
overlapping. Each started at a one-minute load of 3.97–7.56, below the 8.0 threshold.
The 60 MP and RAW reference workloads themselves raised the ending load to 13.67 and 10.91;
the batch paused until load settled before starting the next native case.

### Native qualification

The supported Nikon Z6, Fujifilm X100VI and Air 2S manifest was supplied to full verification.
All 81 full-tier components are covered by passing checks: 77 in the full run, the three RAW
panel checks replayed against their captured state/logs/images with the current action-record
expectation, and a background hardening run with the current live-session registry
expectation. The full command's own summary records a failure; it is not a clean full-run
exit. The editor binary is the same across those checks. The coverage index is
`artifacts/pr5-harness-rechecks-20261008/qualification.json`, the full evidence
`artifacts/pr5-full-final-20261008/summary.json`.

The whole GPU corpus passes the release gate: 305 stacks, 1,220 picture-at-rest
view cells at Fit, 33%, 50% and 100%, and 305 histogram, sample and export checks, with source
hashes unchanged. The four grading recipes cover 28 stacks and 112 view cells. Their largest
picture-at-rest mean / worst block / p99 is 0.0762 / 0.3550 / 0.6469 ΔE00, with absolute signed
mean ΔL* at most 0.0092. Their largest histogram earth mover's distance is 0.249659 code,
within 0.25; samples and repeatable GPU exports pass their own limits. Motion is reported,
not gated: 48 of grading's 84 reduced-view cells exceed the comparison
limits, against both the settled picture and the reference; all 28 at 100% are within them.
The whole corpus has 572 reduced-view motion cells past a limit.
The report is `gpu-qualification/run/report.json` inside the full evidence directory.

### Native control latency and resources

`editor-latency --samples 30 --mode drag|commit --action set-mixer --parameter FIELD`
measures HSL `red-hue`, grading `grade-shadows-luminance`, and the Shadows wheel
(`--control wheel --parameter grade-shadows-hue`); drag runs use `--warm 1000`.
Every run launches a hidden background window at Fit, 1440 × 900 logical points,
2880 × 1800 physical pixels. Each scalar run activates its named field; each wheel run changes
hue and saturation together in the selected, expanded Shadows view. These are separate control
workloads, not an additive HSL-plus-grading comparison. All 270 drained drag inputs were drawn
on the GPU, none held or answered by the reference. Each interval starts at `draft.set` and ends
at the first draw tagged with that revision; it excludes pointer-device latency and display
scanout. Commit frame and histogram distributions each contain 30 complete gestures. A drag
run has two releases, including its coalesced reverse burst: those two histogram observations
are listed as a range, not treated as a representative tail distribution.

| Photo / control | Drag p50 / p95 ms | Commit frame p50 / p95 ms | Commit histogram p50 / p95 ms | Drag release observations ms (n = 2) |
| --- | --- | --- | --- | --- |
| 24 MP JPEG / HSL red hue | 8.14 / 9.26 | 9.18 / 9.47 | 67.55 / 68.14 | 66.01 / 75.40 |
| 24 MP JPEG / Shadows luminance | 8.24 / 8.45 | 9.20 / 9.53 | 67.35 / 69.10 | 65.72 / 330.58 |
| 24 MP JPEG / Shadows wheel | 8.71 / 9.01 | 9.81 / 10.18 | 67.40 / 68.13 | 69.89 / 76.17 |
| 60 MP JPEG / HSL red hue | 8.21 / 9.38 | 9.23 / 9.59 | 142.33 / 143.28 | 148.10 / 150.91 |
| 60 MP JPEG / Shadows luminance | 8.25 / 9.32 | 9.23 / 10.12 | 142.52 / 150.60 | 139.80 / 150.93 |
| 60 MP JPEG / Shadows wheel | 8.69 / 9.04 | 9.78 / 10.66 | 142.59 / 150.24 | 145.24 / 150.19 |
| Nikon Z6 RAW / HSL red hue | 8.31 / 9.36 | 9.13 / 17.45 | 67.81 / 83.79 | 72.20 / 352.04 |
| Nikon Z6 RAW / Shadows luminance | 8.31 / 8.45 | 9.21 / 16.82 | 67.67 / 84.55 | 71.79 / 369.25 |
| Nikon Z6 RAW / Shadows wheel | 8.71 / 9.02 | 9.73 / 10.08 | 67.12 / 68.03 | 69.97 / 76.12 |

Every measured drag p95 is below 16 ms. Scalar RAW commit tails reach 17.45 and 16.82 ms;
this does not establish every commit within one display frame. The 24 MP grading-luminance
release at 330.58 ms misses the provisional 200 ms histogram target, despite the separate
30-commit distribution's 69.10 ms p95. RAW HSL and luminance releases reach 352.04 and
369.25 ms. Those sparse release misses are findings.

The resource rows below take the larger observed value across each control's drag and commit
runs. RSS is the process sampler's peak at roughly 50 ms intervals, including captures, GPU
resources and allocator retention, not a CPU-heap measurement. GPU allocation is the final
native counter, not its peak; on unified memory it must not be added to RSS. CPU seconds cover
the observed process interval, including setup, captures and commits, not just shader execution.

| Photo / control | Sampled peak RSS MiB | Final GPU allocation MiB | Process CPU seconds |
| --- | ---: | ---: | ---: |
| 24 MP JPEG / HSL red hue | 1020.3 | 379.2 | 4.75 |
| 24 MP JPEG / Shadows luminance | 1022.8 | 379.0 | 4.69 |
| 24 MP JPEG / Shadows wheel | 1213.2 | 377.0 | 5.06 |
| 60 MP JPEG / HSL red hue | 1111.0 | 562.9 | 6.16 |
| 60 MP JPEG / Shadows luminance | 1125.4 | 566.6 | 6.31 |
| 60 MP JPEG / Shadows wheel | 1362.0 | 514.9 | 6.44 |
| Nikon Z6 RAW / HSL red hue | 1468.9 | 576.6 | 8.59 |
| Nikon Z6 RAW / Shadows luminance | 1466.2 | 576.6 | 8.78 |
| Nikon Z6 RAW / Shadows wheel | 1673.6 | 574.5 | 9.13 |

These repeated-capture gesture workloads exceed the provisional 600 MiB RSS target at 24 MP
and 1 GiB at 60 MP, HSL baselines included. They do not establish a production working-set
ceiling or isolate the wheel's own allocation cost. Colour scratch peaks are zero on the JPEGs
and 5.25 MiB on the RAW. The GPU and scratch limits are the declared ones; backend staging
and total GPU memory outside the declared services are not bounded by these observations.

### Paired whole-frame reference costs

The figures below were taken with independent owners run sequentially, preparing the original once
per owner, with one warm-up and 30 samples per workload. `grade-performance` alternates the two
workloads sample by sample on one owner ([development](../engineering/development.md)), so its
figures are not directly comparable with these.
HSL holds red hue 20/21 (alternating to make every commit real) and blue saturation 15.
The graded workload keeps those HSL values and adds Shadows hue 210/saturation 30,
Highlights hue 45/saturation 20 and Global luminance 5. Commits use `edit.set-mixer`;
whole-frame rendering uses the CPU reference; each centre-point `render.sample` equals that
frame's byte; each serial `export.write` completes and its temporary output is removed.
The headless owner has no GPU tile service, so sample and export figures below are reference
fallback costs, not native GPU sample/export throughput. Every table cell is p50 / p95 over 30
observations; RSS is sampled between operations, not peak memory.

| Photo / reference workload | Owner commit ms | Whole-frame render ms | Point sample ms | JPEG export ms | RSS between operations MiB |
| --- | --- | --- | --- | --- | --- |
| 24 MP JPEG / HSL alone | 4.81 / 6.47 | 92.10 / 101.06 | 0.092 / 0.108 | 154.20 / 179.73 | 235.2 / 235.5 |
| 24 MP JPEG / HSL + grading | 4.91 / 5.67 | 148.47 / 163.38 | 0.091 / 0.107 | 209.88 / 237.04 | 236.2 / 236.4 |
| 60 MP JPEG / HSL alone | 4.86 / 5.66 | 214.41 / 238.58 | 0.095 / 0.114 | 347.33 / 381.85 | 508.0 / 508.2 |
| 60 MP JPEG / HSL + grading | 5.21 / 5.88 | 373.10 / 400.28 | 0.103 / 0.119 | 511.87 / 521.36 | 512.7 / 512.8 |
| Nikon Z6 RAW / HSL alone | 4.89 / 6.30 | 382.50 / 420.13 | 0.140 / 0.157 | 467.38 / 520.45 | 539.0 / 539.2 |
| Nikon Z6 RAW / HSL + grading | 4.71 / 5.43 | 487.76 / 510.51 | 0.147 / 0.167 | 574.52 / 589.40 | 464.4 / 464.4 |

Adding this grade raises the reference render's p50 by 56.37 ms at 24 MP, 158.69 ms at 60 MP,
and 105.25 ms on the Nikon RAW. Samples remain pointwise. The lower graded RAW RSS is an
observation across sequential owners and allocator retention, not evidence of a memory saving.
All originals retain their hashes. Reports preserve recipes, source hashes, export jobs, loads,
CPU/GPU resources and native state/logs/captures under
`artifacts/pr5-measurements-20261008/`; `summary.json` indexes all 21 reports and `runs.json`
retains the exact commands. JPEG sources are the generated 6000 × 4000 and 10000 × 6000
fixtures; the Nikon's upright stage is 4024 × 6048.

The full run's general 24 MP `editor-performance` diagnostic is also retained, at its default
10 samples per recipe and starting load 3.73; it is a diagnostic, not a feature-cost comparison,
and the paired HSL/grade workloads above are the feature-cost comparison. Native Windows/Linux,
screen-reader, calibrated-display and Lightroom response qualification remain open.

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

Least squares over the 37 sampled counts gives `S(n) = 19.30·n² + 1623·n + 2511` bytes, identical on both fixtures. Doubling the stroke count multiplies the stored bytes by 3.49 at 250 → 500 and 3.71 at 500 → 1000: **the growth is quadratic and nothing here may be described as linear.** The measured 20.9 MB at 1000 strokes is the 35-byte reference's curve plus each entry's own JSON and the mask and component structure the references hang on.

**Two ceilings, both measured.** Sixteen masks of 8192 stored positions hold 1809 of these strokes and no more (`a_session_of_long_strokes_ends_at_the_masks_per_recipe_ceiling`). Independently of stroke length, the 256 KiB per-recipe serialized mask bound is reached at 7040 stroke references, which is the most any recipe can hold; past it the write is refused with `resource-limit: recipe masks serialize to 264356 bytes; the limit is 262144 serialized mask bytes per recipe` and the catalog is unchanged, proved by its SHA-256 before and after and by reopening to the same current entry and history length (`a_recipe_over_the_serialized_mask_bound_names_it_and_leaves_the_catalog_as_it_was`). No painting session's snapshot therefore carries more than 256 KiB of mask data.

**The hash-chain variant recorded in the design is not needed and stays unbuilt.** A realistic long retouching session of a few hundred strokes costs one to six megabytes; the largest session of usable strokes that can exist costs 66 MB; and the pathological maximum — 7040 single-position strokes, one entry each, every snapshot at the 256 KiB bound — is at most 1.7 GB, which is a bound and not an open end. Nothing measured here asks for a variant that would cost an O(strokes) walk to rebuild a stroke list on every read.

**Reopen and peak memory are timings on a shared host and are provisional.** The one-minute load average was 20.8 to 25.1 during these runs — far above the 8.0 at which a figure stops being quotable as a baseline — so they are reported as a ratio and an order of magnitude, not as a target. Reopen is `EditorService::open` plus `state` over the 1809-stroke catalog, three consecutive rounds, and the spread within each triple was under 0.2 ms, so the numbers are stable *under that load* even though the load makes their absolute level unreliable.

| Fixture | Reopen (3 rounds) | Load | Peak RSS | Peak footprint |
| --- | --- | --- | --- | --- |
| 24 MP | 14.2 / 14.2 / 14.1 ms | 25.1 | 271 MiB | 266 MiB |
| 60 MP | 21.2 / 21.1 / 21.0 ms | 20.8 | 651 MiB | 647 MiB |

Peak memory is the whole test process, which imports and decodes the fixture. Both processes wrote the identical 66 MB catalog, so the 380 MiB between the two rows is the 36 MP between the two images and nothing else: the history itself is not resident, because entries are written and read one at a time and never held together. Reopen resolves all 1809 stroke references and grows by about 7 ms between the two sizes, which is the source decode and not the store.

### Commit time: writing only fresh strokes

`store_strokes` writes only the references a recipe's stroke table does not already know are durable — known because a fresh hydration marked them, or because an earlier commit built on the same table already wrote them — so a long session's later commits pay only for the strokes each one actually captured, not one write per reference the whole mask table carries.

The figure below is a controlled A/B within one binary: the same 1809-stroke ceiling session, with the harness's marking of each commit's fresh stroke as stored on (the fresh-only write) and off (every reference treated as needing a write on every commit), run in reversed order (fresh-only, every reference, every reference, fresh-only) on a host shared with other sessions.

Scope: the mean write-transaction time (`insert_entry` plus the asset-state update) of the last 50 of the 1809 commits, native Apple M4 Pro, release `--locked`, `luxforge-core`'s own catalog, on the generated 24 MP and 60 MP fixtures. Catalog bytes are the same either way — the table above — because `INSERT OR IGNORE` guards every write; only the amount of SQL a commit issues differs.

| Source | fresh-only, ms | every reference, ms | one-minute load |
| --- | --- | --- | --- |
| 24 MP | 0.824, 0.815 | 3.114, 3.586 | 11.37 → 10.16 |
| 60 MP | 0.816, 0.993 | 3.022, 3.307 | 7.16 → 6.83 |

The difference survives the reversal on both fixtures: 0.8–1.0 ms of write-transaction time per commit near the ceiling, against 3.0–3.6 ms writing every reference. The gap widens over a longer session, since writing every reference costs one write per reference the whole mask table carries — which grows with the session — while the fresh-only cost is one write per stroke the commit itself captured, which does not.

### Point samples through a spatial layer

A sample through a spatial layer is a read by the catalog owner's tile service, which keeps the read off the owner ([samples from GPU tiles](#samples-from-gpu-tiles)). The release gate compares samples read from GPU tiles with the reference renderer's frame, within the display limit rather than to the byte; on the private RAW sources (a selection, so the run reports itself incomplete):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind sample --families presence \
  --sources raw-z6,raw-x100vi,raw-air2s
```

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

The XML parser checks each element's attributes against each other, so its cost grows with the square of an element's attribute count. The prescan bounds that work to 2,000,000 comparisons; unbounded, the first shape took 4.1 s p50 and 7.4 s max. The prescan also bounds nesting, which the parser descends recursively, and namespace declarations, which it scans for every prefix.

## Module capabilities qualification

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, release `--locked`, on a host shared with other sessions: the one-minute load is given per measurement. No figure here is a p95 claim beyond its stated sample count.

### The framework's own costs

`cargo test --release --locked -p luxforge-core --lib capability_timing -- --ignored --nocapture`, isolated directories, an in-memory secret store and the loopback proof endpoint; load 10.9 at the start. The p50s recorded here are the upper median (the 16th of 30, the 101st of 200); the shared nearest-rank `Distribution` the test reads gives the 15th and the 100th. The p95s are the same either way.

| Measurement | p50 / p95 | Samples |
| --- | --- | --- |
| Registration, the eight built-ins | 0.020 / 0.022 ms | 200 |
| Registration, built-ins and the proof module | 0.031 / 0.035 ms | 200 |
| `module.status` owner round trip | 0.013 / 0.023 ms | 30 |
| `module.settings.read` owner round trip | 0.009 / 0.012 ms | 30 |
| A whole `task.generate-proof-tint`: request to `ready`, including the 64 samples, the file read, the loopback request, the artifact publish and its row | 14.9 / 15.8 ms | 30 |
| … of which publishing one 12-byte artifact (synced, renamed) | 9.1 / 9.9 ms | 30 |
| Cancel a task stalled inside its request to `cancelled` (the cancel shuts the request's socket down; measured on the `ureq` transport, load 35 to 39) | 0.28 / 0.89 ms | 10 |
| Installed proof resource on disk, `installed.json` included; staging left behind | 448 bytes; none | 1 |

The two request rows were measured through the real transport against the loopback proof endpoint. `capability_timing` sends through the core's in-memory transport, which checks a cancel every millisecond, so a run of it measures the host around the request, not the network path; the real transport's cancel latency is the one in the [module capabilities design](../design/module-capabilities.md#transport).

Discovery, registration and catalog reopen start no worker thread and create no directory: `schema.list` and `module.list` against a fresh data root leave it empty, and the owner tests assert that no lane has started and the secret store saw no call. The two lanes block on their channels while idle.

## Performance section, activity board and resource counters

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Rust 1.94.0, release builds.
The `resources_cost` and GPU-walk p50s are the upper median (the 501st of 1000 reads, the 101st
of 200 walks); the shared nearest-rank `Distribution` those probes read gives the 500th and the
100th. Their p95s are the same either way.

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

Ordinary-session idle: `editor-latency --source fixtures/generated/24mp.jpg --mask --samples 30
--idle`, two runs. Each run reopens its committed masked Basic stack without evidence mode, settles one
second, then measures process CPU-time delta for 30 seconds. Performance is expanded; its
one-second sample and whole-window redraw are included. Native GPU accounting comes from the
preceding gesture's captured state, not an idle-process capture. No local build or test overlaps.

| Run | CPU, % of one core | Duration, s | Sampled peak RSS, MiB | Idle start → end one-minute load |
| --- | --- | --- | --- | --- |
| 1 | 0.827 | 30.239 | 468.3 | 7.76 → 6.81 |
| 2 | 1.190 | 30.264 | 374.5 | 6.58 → 5.86 |

Both windows stay below the quiet threshold of 8; one exceeds the provisional 1% idle target.
RSS includes allocator retention and GPU resources and sets no memory claim here. There is no
paired collapsed window, so these numbers do not isolate the section's extra CPU. The remaining
toolkit view, layout and whole-window redraw cost is still open. Raw reports are in
`artifacts/mask-perf-20261002/idle-*/resources.json` and `comparison.json`.

The isolated release probe `app::performance::tests::performance_model_work` compares the
full-workspace derivation route with the scoped refresh on the same imported 24 MP
photo. Each route receives a changed resource sample, with a full 61-sample history; order alternates
within each pair. After 200 warm-up pairs, 1000 samples per route give full derivation p50/p95
60.38/64.21 µs and scoped refresh 1.46/1.58 µs: about 98% less model work. Pixel workers are stopped
before timing; import, decoding, owner calls, widgets, layout and GPU work are excluded. This is
a component comparison, not an end-to-end redraw or ordinary-session CPU reduction. The shared
host's one-minute load is 8.64. Every sample is retained in `model-work.json` beside the idle reports.

## Isolated rendering kernels

The Presence scalar accessor is inlined, RAW terminal conversion uses the sRGB code-boundary table
with the forward conversion within `1e-12` linear of a boundary, and Basic's skin-hue weighting
uses its bounded atan2 input directly without periodic normalization. These kernels keep tile
geometry, filter arithmetic, scratch targets, scheduling and image semantics. The boundary fallback
keeps the forward rounding in the native exactness checks; cross-platform numerical qualification
remains open.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0 release, generated 6000 × 4000
and 10000 × 6000 inputs, 30 samples per variant and size, with one warm-up per process. These are
complete core renders, including the output allocation, from already-prepared data; JPEG decoding,
RAW decoding/demosaicing, proxy creation, histogram reduction and desktop presentation are outside
the timed region.

| Workload | p50 / p95 |
| --- | --- |
| 24 MP, Texture +100, Clarity +100, Dehaze +100 | 664.1 / 791.1 ms |
| 60 MP, Texture +100, Clarity +100, Dehaze +100 | 2877.7 / 2927.0 ms |
| 24 MP, Vibrance +50 / Saturation +20 | 84.6 / 88.2 ms |
| 60 MP, Vibrance +50 / Saturation +20 | 214.7 / 237.3 ms |

One-minute load was 5.0–8.0 for the Vibrance rows and 12.2–15.2 for Presence, including the
benchmark itself; the Presence results exceed the harness's 8.0 load threshold. These are not an
absolute latency budget or a passed responsiveness target. Presence's 60 MP median CPU time per
15-sample leg is 33.3–33.4 CPU-seconds. Full Basic, prepared rendering and the proxy are measured
in [prepared rendering and native DNG development](#prepared-rendering-and-native-dng-development).

Independent filter, tile, masked sample/render and quantization-boundary references cover these
kernels. The [Markesteijn row executor](../design/native-demosaic-parallelism.md) preserves tile
equations and bounds scratch without adding another thread pool; its measurements are
separate in [startup and RAW throughput](#startup-and-raw-throughput).

The release `editor-performance` diagnostic, 30 samples per variant, includes its composed
transforms and 10° crop, so these values are separate from the bare kernel rows above. All
original-source hash checks pass. One-minute load at the start of a leg ranged from 4.99 to 15.87;
these are not absolute budgets.

| `editor-performance` workload | p50 / p95 |
| --- | --- |
| 24 MP source, Vibrance/Saturation + geometry | 65.8 / 69.4 ms |
| 60 MP source, Vibrance/Saturation + geometry | 157.7 / 161.3 ms |

The existing native Presence sample/render test passes on the Nikon, Fujifilm and DJI originals,
for 41 points including the far corner through each of Clarity and Clarity plus Dehaze. Those test
times are not a latency distribution.

The local evidence is under `artifacts/performance-first-wave/`; the kernels' contract and
performance review checklist are [below](#the-isolated-kernels-contract-and-review).

### The isolated kernels' contract and review

These CPU kernels preserve photo output, editing semantics, recipes, API shapes, scheduling and
memory targets.

**Presence.** The filters' plane accessor (`Plane::get`) carries an ordinary inline hint. The
release compiler can fold its coordinate clamps and address arithmetic into the callers; on the M4
no out-of-line calls to this accessor remain. Edge clamping, pixel arithmetic, f64 filter
accumulation, the tiling, parallelism and scratch bounds are independent of it, and it adds no
specialized assembly or architecture-specific path.

**RAW terminal conversion.** Finite values are clamped at the terminal boundary and quantized
through the static sRGB code thresholds. Within `1e-12` linear of either adjacent code boundary,
the forward power/round evaluation is kept: forward and inverse floating-point transfer
functions can disagree on the final byte at a boundary. Non-finite rejection and the extended
linear domain before terminal conversion are kept; JPEG quantization keeps its own contract.
The guard is conservatively tested on the M4, not a formal cross-platform error bound for `powf`.

**Basic hue weighting.** The private skin-hue response accepts the bounded angle from atan2.
Subtracting its 55° centre gives `[-235°, 125°]`. The only part that would wrap lands in
`[125°, 180°]`, outside the ±35° active band either way, so the response uses the delta directly,
with no general normalizer or remainder, and the same band test and cosine arithmetic.

**Exactness.** Independent filter references, serial/pool comparisons, tiles, masks and
sample/render parity cover Presence. RAW tests compare the forward terminal conversion
at all 255 code boundaries and 128 f64 neighbours on either side, at the guard edges, through a
dense sweep and deterministic float bit patterns, and on signed, headroom and non-finite inputs.
An independent periodic oracle checks the hue response's boundaries and a million bounded angles.
Kernel timings are not presented as desktop latency or demosaic speedups, and native
Windows/Linux numerical and desktop qualification remain separate from the M4 evidence.

| Review question | Answer for these kernels |
| --- | --- |
| Original reads, hashes and decodes | No request paths of their own. Preparation goes through the signature-verified source cache. |
| Frame allocations and sharing | No buffers of their own. Output allocations, identity sharing and spatial/colour targets remain in force. The RAW quantizer uses the static threshold table. |
| Point queries, validation and no-op checks | No rasterization of their own. Spatial sampling keeps its declared one-tile exception and exact render parity. |
| Catalog owner work | No work moves to the owner. |
| Desktop messages and uploads | No changes to state/history refresh, preview jobs or uploads. |
| Timers, polls and subscriptions | None. |
| Photo-sized cost | Release evidence is recorded above, with kernel diagnostics distinguished from `editor-performance` and native desktop journeys. |
| Exactness and sharing tests | Presence keeps the independent filter references, serial/pool, tile, mask and sample/render checks. RAW tests compare the forward conversion at every code boundary and across finite/non-finite inputs. Hue tests compare the periodic formula at boundaries and through a million bounded angles. Identity-sharing tests apply. |

### Straightened-crop resample

The byte resample's bilinear sample rounds each channel forward, `round(255 · encode(v))`, through
the same guarded threshold search as the RAW terminal (`srgb::Quantizer::rounded`): the code-boundary
index answers every value more than `1e-12` linear from both thresholds around its code, and the
forward transfer function answers the rest. A 10° crop of the 24 MP fixture outputs 3695 × 2077, so
this avoids about 23 million `powf` calls in the exact render. The colour row loops, the byte
proxy build, the byte spatial entry's read and the RAW terminal rows take the decode table and the
quantizer once per pass, the linear point path reads its source through the view resolved once at
construction, and the finiteness scan after each colour unit does not short-circuit, so it
vectorises.

Byte identity: the boundary tests drive `bilinear` itself across every code threshold, with
horizontal and two-row blends of three corner pairs per threshold stepped ULP by ULP through and
around the guard band, plus 200,000 random frames and taps; the quantizer without its guard fails
them.

`editor-performance --source fixtures/generated/24mp.jpg --samples 30`, release `--locked`, native
M4 Pro, warm cache, two rounds of two legs each on a heavily shared host (one-minute load 12.0 to
15.6 at the start of a leg). p50 of each leg, in run order:

| 24 MP, p50 ms | Round 1 | Round 2 |
| --- | --- | --- |
| 200 transforms and a 10° `crop-fit` | 22.0, 29.7 | 18.9, 22.9 |
| The same, less the 200-transform row (the crop's own cost) | 12.9, 18.1 | 10.5, 15.5 |
| Proxy frame of the crop stack (2879 × 1618) | 11.9, 12.8 | 10.5, 11.7 |
| Proxy frame of the crop stack with a full Basic layer | 47.9, 45.6 | 42.8, 45.0 |

The straightened crop's own cost over the unrotated stack is 11 to 18 ms at the median, and the
proxy frame of the crop stack 11 to 13 ms. At this load these are not absolute budgets.

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

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, release with locked pins.
All agent builds/tests were paused during timing. Each reported distribution has 30 observations
per variant, with no tails removed. Kernel leg-start one-minute load was 10.2–16.9, including the
benchmarks themselves, above the harness's 8.0 threshold. These are live-host figures, not passed
absolute latency budgets. Raw observations, commands, hashes and load are retained locally in
`artifacts/performance-second-wave/`, and the research evidence in `artifacts/performance-next/`.

### Source preparation and rendering: contract and review

Source preparation and rendering keep the image and editing contracts and use no approximation.
Originals stay immutable, no pixel work runs on the owner or UI thread, and the user-facing API,
recipe and catalog shapes are those of the ordinary commands. One active preparation/preview lane
and the resource limits hold, and parallel work shares the process pool, measured under contention
as well as in isolation. The source design contracts are [initial RAW](../design/initial-raw.md),
[Air 2S](../design/air2s-dng.md), [Presence](../design/presence-mixer-vignette.md) and
[instant previews](../design/instant-preview.md).

1. A cold known RAW develops once, at the validated requested white balance; a new import keeps
   as-shot. Fingerprints, source interpretation, historical entry identity, concurrent changes and
   generation adoption stay authoritative.
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

**Exactness.** Exact numerical paths are checked against independent canonical references,
representable neighbours of every relevant threshold, error and cancellation cases and complete
photo-sized output comparisons. Source preparation is exercised through new import, current and
historical saved white balance, reload, interpretation and fingerprint mismatch and a concurrent
white-balance change. Spatial estimates prove that preparation and reduction are actually avoided
and correctly invalidated. DNG compares serial and pool output and supplied-file samples. The
overlay compares every cell against a serial oracle. RAW rows compare generic evaluation, samples
and buffers across views and exposure.

**Review checklist.**

- Reads, hashes and decode stay on the signature-verified source worker; direct-at-target white
  balance develops once.
- The quantizer adds a bounded static table; DNG keeps one bounded scratch plane; clipping makes no
  transient display-grid copies and bounds tiny-grid scratch to 4 MiB; the RAW row path adds no
  frame-sized scratch.
- Samples stay exact and use the bounded spatial tile; skipping irrelevant estimates
  removes work without rasterizing a frame.
- The owner performs only catalog, identity and validation work and job coordination; no owner
  frame work.
- Desktop state, history, preview and upload paths and timers are unaffected.
- Output sharing and resource limits are kept and checked by targeted tests.

### Prepared rendering and native DNG development

Core render times include output allocation/drop and exclude source preparation, histogram reduction,
queue delay and presentation. Generated JPEGs are 6000 × 4000 and 10000 × 6000. Full Basic uses ten
non-neutral fields; Mixer uses red hue +30, orange saturation +20 and blue luminance −30. The proxy
is 2700 × 1800, prepared outside timing. Only the Air 2S row develops an actual retained native
mosaic, including required optical corrections; it excludes file read/unpack.

| Workload | p50 / p95 |
| --- | --- |
| Air 2S, native retained-mosaic development | 350.3 / 360.9 ms |
| 24 MP full Basic | 110.8 / 120.3 ms |
| 60 MP full Basic | 277.6 / 284.8 ms |
| 24 MP Mixer | 112.0 / 136.6 ms |
| 60 MP Mixer | 268.2 / 275.7 ms |
| 24 MP source, full Basic proxy | 22.8 / 24.8 ms |

Serial/pool DNG tests preserve every float bit, stage/channel order, active-area borders, failures
and cancellation. A separate ten-observation threshold diagnostic gives median 56.7 / 6.8 ms
serial/pool at one megapixel and 170.3 / 19.5 ms at three megapixels for synthetic
gain/warp/vignette; inputs below one megapixel remain serial. This small diagnostic is threshold
feedback, not a p95 distribution.

### Spatial amount edits and clipping

Texture and Clarity need no reduction, even on the first sample after a strength edit; Dehaze builds
an atmosphere only on a cold cache or a changed input.

The clipping rows include reduction and output allocation, excluding decode, painting, upload and
queueing. A 4096 × 2458 grid holds 9.6 MiB; it is not replicated per parallel fold.

| Workload | p50 / p95 |
| --- | --- |
| 24 MP → 1716 × 1144 grid | 1.9 / 2.8 ms |
| 60 MP → 1716 × 1030 grid | 4.3 / 4.5 ms |
| 60 MP → 4096 × 2458 capped grid | 4.3 / 4.8 ms |
| 60 MP → 1 × 1 grid | 4.4 / 4.6 ms |
| 60 MP → 8 × 8 grid | 4.4 / 4.7 ms |

### Integrated editor diagnostic

The official `editor-performance` diagnostic uses 30 observations per variant and size. These
recipes include the harness’s composed transforms and 10° crop, so their values are separate from
bare kernels. Source-preservation checks pass. Leg-start load was 7.1–15.4; geometry/histogram
medians vary by roughly 0–4% between runs, so smaller shifts are not attributed to a change. Full
Basic rows are in [shared-pool contention and rendering controls](#shared-pool-contention-and-rendering-controls).

| Workload | p50 / p95 |
| --- | --- |
| 24 MP source, Exposure + geometry | 31.0 / 33.1 ms |
| 24 MP source, Build display-bounded proxy | 14.8 / 19.7 ms |
| 60 MP source, Exposure + geometry | 72.0 / 78.2 ms |
| 60 MP source, Build display-bounded proxy | 24.4 / 30.0 ms |

### Cold saved-white-balance preparation

A cold RAW whose catalog holds a saved custom white balance takes one source job, developed at that
white balance. Native correctness regressions cover the Nikon Z6, a new as-shot import, two distinct
custom historical/current WB targets, source and interpretation mismatch, same-target
deduplication, different-target cancellation and a WB edit while loading. Old gains never satisfy a
strict request for the newer edit. The method is in
[native development and saved-white-balance preparation](#native-development-and-saved-white-balance-preparation)
and the current figures in
[cold saved-white-balance preparation, LibRaw and RawSpeed](#cold-saved-white-balance-preparation-libraw-and-rawspeed).

### Native qualification

Native Presence sample/render tests pass on the Nikon, Fujifilm and DJI originals. DJI reopen,
Presence and mask/range captures are visually inspected alongside state and event checks.
Windows/Linux numerical and native GPU qualification are not established by this M4 evidence.

## Startup and RAW throughput

Initial-source preparation overlaps platform startup. RAW camera conversion mutates the existing
planes in exact bounded chunks, mosaic normalization and the output scale use bounded 16-row jobs
above one megapixel, and native Markesteijn and Bayer RCD use bounded tile jobs on the shared pool.
See the [contract and review checklist](#startup-and-raw-throughput-contract-and-review) and
[native execution bounds](../design/native-demosaic-parallelism.md). No image equation or output
quantization tolerance is relaxed.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, release with locked pins. Each
distribution has 30 observations per variant, with tails retained. Builds and tests stop during
timing. Commands, hashes, load, raw samples and captures are retained locally in
`artifacts/performance-third-wave/`. Scopes below overlap and their figures must not be added.

### Startup and RAW throughput: contract and review

This work relaxes no numerical contract and adds no runtime dependency or scheduler. Source
timings, render kernels and first-frame timings are distinct, and they overlap.

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
   reject non-finite input. Cancellation and arithmetic order are preserved, with no extra
   full-frame allocation.

**Exactness and liveness.** Native complete float buffers agree byte for byte with the serial
reference at one, two, four and the admitted maximum workers, including custom white balance, odd
edge tiles and concurrent callers; original hashes, RAW history and reopen, and sample/render
parity stay intact. Worker count and aggregate explicit scratch are bounded. Cancellation, failures
and teardown publish no partial frame, leak no work and never unwind across FFI, and no callback
outlives the native call. Startup keeps the ordinary open behaviour: missing or invalid files report
normally, a replacement cancels stale work, one source job is adopted once, an existing saved white
balance is respected, and headless and background behaviour is unaffected. Unsupported platforms and
absolute targets under load remain unqualified.

**Review checklist.** Source work stays on the bounded worker. The native executor borrows the
immutable mosaic and context, has disjoint output interiors and reuses admitted per-callback scratch
across its tiles, then joins before borders. The core conversion mutates its existing three planes
in place. Startup overlaps an existing job rather than creating a second service or cache. Pixel
arithmetic, default previews, exact analysis and API/UI history keep their contracts.

### Exact camera conversion

The timer covers camera-to-working-space conversion and adoption validation over actual developed
camera planes, through the private producer and adoption boundary. Allocation/clone, file
read/decode, native development, hashing and drop are outside it. Leg-start load was 3.0–5.5.

| Camera | p50 / p95 |
| --- | --- |
| Nikon Z6 | 3.74 / 4.63 ms |
| Fujifilm X100VI | 6.04 / 7.62 ms |
| DJI Air 2S | 2.98 / 3.91 ms |

The conversion writes disjoint 65,536-pixel chunks in place, as jobs of the development executor
([native demosaic parallelism](../design/native-demosaic-parallelism.md#why-callbacks-are-bounded)) above one
megapixel and serial chunks below. Each output is checked finite at production; private adoption
retains shape/capacity checks without another full scan. Public constructors scan and reject
invalid input. There is no additional full-frame scratch.

### First image during startup

These are app-cold, filesystem-warm launches of temporary copied background bundles with a hidden
Metal window. An independent 10 ms event-file observer measures from argument parsing to the first
observed proxy event. This includes event observation delay; it is not scanout, foreground activation
or a stable installed-bundle launch. Leg-start load was 4.2–5.9.

| Source | p50 / p95 |
| --- | --- |
| Generated 24 MP JPEG | 764.0 / 778.6 ms |
| Generated 60 MP JPEG | 778.6 / 806.3 ms |

The authorized import starts before platform initialization and is adopted through the ordinary
open lifecycle; no second source job is created. Empty args-to-observed-capture is 787.5 / 809.2 ms,
and args-to-editor-startup about 578–582 ms. The initial-open request clock starts before
prequeueing.

### Native development and saved-white-balance preparation

Cold saved-WB preparation restarts the owner and source cache per observation, with the filesystem
warm and the catalog's saved red gain at 1.1 × as-shot. Time runs from `catalog.import` until a
strict exact-source `PreviewJob` is available: read/hash/unpack/develop/matrix/validation and
waiting/adoption are included. Owner startup/catalog open, final float hashing, rendering and
presentation are excluded. Every observation's complete float planes and source identity match,
and there is exactly one source job in every sample. The current figures, with LibRaw and RawSpeed
unpacking, are in
[cold saved-white-balance preparation, LibRaw and RawSpeed](#cold-saved-white-balance-preparation-libraw-and-rawspeed);
retained-mosaic development is measured in [mosaic normalization](#mosaic-normalization).

The X100VI's retained-mosaic development has a process CPU median of 1512–1517 ms per 15-sample
leg. Peak process RSS from the whole diagnostic process is 933.2–933.4 MB (decimal bytes); this
includes preparation and measurement, not just live demosaic scratch.

Explicit Markesteijn heap scratch is capped at eight × 988,208 bytes, or 7,905,664 bytes, for the
one development the source worker runs at a time, including the source-caller job. Stack arrays,
tables, allocator overhead and full image buffers are additional. Callback cancellation, native faults, nested callers and teardown are tested;
no partial output is adopted.

### Bayer RCD tile jobs

RCD runs its 194 px tiles as jobs of the same executor as Markesteijn, checking cancellation before
every tile ([native demosaic parallelism](../design/native-demosaic-parallelism.md)). Each job holds
one 978,536-byte scratch set under the same eight-lane cap. Complete RGB planes match the serial
raster at every worker count on the Z6 and Air 2S.

Native M4 Pro, 14 cores, release, one-minute load 3.2 to 4.1. Each run is the crate's
`bayer_normalization_release_abba_profile`: 30 warm observations in one fresh process, two runs per
camera. The executor arm is the production path. Timing covers retained-mosaic development:
normalization, demosaic, output allocation and the final divide; it excludes source read and
decode, the DNG corrections, rendering and presentation.

| Source | Wall p50 / p95 | Demosaic p50 / p95 | Process CPU p50 / p95 |
| --- | ---: | ---: | ---: |
| Nikon Z6 | 55.9 / 58.3, 56.0 / 62.2 ms | 40.8 / 42.9, 40.8 / 44.9 ms | 301.3 / 307.2, 300.5 / 319.1 ms |
| DJI Air 2S | 44.2 / 45.1, 44.3 / 46.1 ms | 30.7 / 31.0, 30.6 / 31.1 ms | 256.0 / 262.3, 257.4 / 262.6 ms |

Concurrent Fit proxies against a pooled RCD development are not yet measured. To repeat, run the
profile once per camera:

```sh
LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_PROFILE_SOURCE=nikon_z6.NEF \
LUXFORGE_RAW_TIMING_OUTPUT=/path/to/z6.csv \
  cargo test --release -p luxforge-raw --locked --lib -- --ignored --exact \
  tests::bayer_normalization_release_abba_profile
```

### DNG GainMap taps

A GainMap stage derives each active row's and each active column's bilinear taps (two map indices
and a weight) once, then weighs every pixel of every channel by the same f64 expression in the same
order.

The crate's `owner_development_timing` runs one discarded warm-up, then 30 observations of the
production development followed by its corrections, in one fresh process. Correction time covers
GainMap, WarpRectilinear and the copy back, over the three camera planes developed in that
observation; file read, decode, development, hashing and drop are outside it. The Air 2S's
correction figures are in [development executor refill](#development-executor-refill). To repeat:

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
keep the eight-lane cap; the Rust passes run at the pool's width. Every job starts at a full
tile with fresh scratch, so the planes do not depend on the scheduling.

The Rust passes run on the executor on measurement, not only for the bound. With
`owner_development_timing` as in [DNG GainMap taps](#dng-gainmap-taps), on a shared host, Air 2S
correction p50 over four runs is 83.0, 106.6, 84.7 and 84.2 ms on the executor against 86.5, 92.3,
86.8 and 87.3 ms on a parallel iterator; the one slower executor run has a p95 of 204 ms and ran
during a load spike, so the two are equal or the executor slightly faster. The X100VI camera
conversion in core's `camera_conversion_photo_timing`, 30 samples per run at load 10 to 19, gives a
p50 of 6.66 and 6.19 ms on a parallel iterator and 6.71 and 6.26 ms on the executor, with the same
output digest: equal. The eight-lane cap bounds native scratch, which the Rust passes do not hold:
a camera-conversion job is one 65,536-pixel chunk and a correction job 16 rows.

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
`((sample - black) * scale) * gain` in `f32`. The normalized mosaic
is allocated without being zeroed, each value written once by the job that owns its rows (a
repaired site twice), and released before the output scale; the scale is one pass over the three
contiguous planes. The neutral picker reads the same per-site black model. Synthetic Bayer and
X-Trans mosaics, including a 1203 × 877 X-Trans frame, have pinned digests.

Native M4 Pro, 14 cores, macOS 26.5.2, Rust 1.94.0, release with locked pins,
the crate's `owner_development_timing` as in [DNG GainMap taps](#dng-gainmap-taps): one discarded
warm-up, then 30 observations per run, each run a fresh process. Development covers normalization,
demosaic, output allocation and the final divide, and excludes file read, decode, the DNG
corrections, the camera conversion, hashing and drop. Each camera ran four times in each of two
passes on a shared host; the one-minute load is given for each run.

| Source | p50 / p95 (load) |
| --- | ---: |
| Fujifilm X100VI, first pass | 222.9 / 241.8 (36.6), 227.0 / 254.3 (26.1), 237.8 / 348.6 (19.8), 228.8 / 291.8 (18.7) ms |
| Fujifilm X100VI, second pass | 202.8 / 285.7 (21.4), 232.3 / 338.1 (17.9), 183.8 / 202.1 (13.8), 195.1 / 203.4 (11.9) ms |
| Nikon Z6, first pass | 42.3 / 47.8 (16.3), 43.1 / 45.2 (14.9), 43.4 / 57.9 (14.6), 42.5 / 54.5 (13.9) ms |
| Nikon Z6, second pass | 39.7 / 72.0 (9.1), 37.0 / 37.8 (12.7), 36.9 / 40.1 (12.4), 37.8 / 56.0 (13.5) ms |
| DJI Air 2S, first pass | 35.6 / 39.4 (14.9), 35.1 / 39.5 (16.5), 36.0 / 39.1 (15.8), 33.9 / 37.8 (14.3) ms |
| DJI Air 2S, second pass | 31.4 / 43.7 (10.5), 33.8 / 77.7 (13.2), 31.3 / 50.5 (13.2), 31.7 / 33.4 (22.2) ms |

In the second, less loaded pass development p50 is 183.8 to 232.3 ms on the X100VI, 36.9 to 39.7 ms
on the Z6 and 31.3 to 33.8 ms on the Air 2S. The p95 figures carry the host's load and no tail claim
is made. Concurrent Fit proxies against this development are not measured, in the core or through
the editor.
The per-site table is at most one CFA and black-repeat period, each row widened to at least 64
sites: 396 sites for a 6 × 6 X-Trans period and 128 for a 2 × 2 Bayer one without a repeat pattern.

### Second development

The source cache keeps at most one more RAW development beside its current one, within
`luxforge_raw::RETAINED_DEVELOPMENT_BYTES` (600 MiB), so a switch between two entries at different
white balances is read back from memory instead of redeveloped
([second development](../design/raw-integration.md#second-development)). A completed source job
evicts the cached development only while a preparation of an original or a redevelopment is
queued; a queued artifact read leaves it.

Native M4 Pro, 14 cores, macOS 26.5.2, Rust 1.94.0, release with locked pins.
Each row is the `raw-editor` smoke scenario's edit launch, one background launch per observation,
from `raw-editor-checks.json`: request to the display event (`preview_displayed`, else
`render_ready`) and request to the correlated `frame_captured`. The journey previews the Original
from history and returns to current after six white-balance edits, then does the same pair again;
the first pair must develop the Original's white balance, and the second pair is the
hold-`\` compare once both developments exist. The capture includes the harness's window
readback, which is what the 100% view row costs with no render (its display is about 1 ms).
There are 30 launches on the X100VI and 10 on the Z6 and the Air 2S, on a shared host whose
one-minute load was 8.8 to 30.3 (X100VI), 12.6 to 19.2 (Z6) and 16.1 to 20.3 (Air 2S). Values are
nearest-rank p50 / p95 in milliseconds.

| Source, step | Display | Capture |
| --- | ---: | ---: |
| X100VI, Historical Original | 242.7 / 378.1 | 303.2 / 508.9 |
| X100VI, Return to current | 20.6 / 71.7 | 131.5 / 255.7 |
| X100VI, Original again | 22.3 / 83.0 | 87.8 / 206.9 |
| X100VI, current again | 21.6 / 102.4 | 130.8 / 292.9 |
| X100VI, 100% view | 1.3 / 2.8 | 63.5 / 78.7 |
| Z6, Historical Original | 76.5 / 139.8 | 146.1 / 239.3 |
| Z6, Return to current | 34.4 / 70.0 | 100.8 / 165.6 |
| Z6, Original again | 31.1 / 54.1 | 99.1 / 143.8 |
| Z6, current again | 32.2 / 44.6 | 102.6 / 133.2 |
| Air 2S, Historical Original | 149.0 / 168.6 | 191.8 / 212.7 |
| Air 2S, Return to current | 20.2 / 42.0 | 81.0 / 135.3 |
| Air 2S, Original again | 20.1 / 63.9 | 63.8 / 122.8 |
| Air 2S, current again | 23.3 / 31.2 | 78.2 / 95.1 |

Without a second development every switch redevelops, and the X100VI's p50 is about 250 ms to
display and 310 to 365 ms to capture, above the owner's 150 ms threshold. With it, a switch whose
development the cache holds displays in about 20 ms p50 on every camera (X100VI p95 72 to 102 ms
under this load) and reaches the capture in 88 to 131 ms p50 on the X100VI, of which about 60 ms is
the readback; an undo, which also renders without redeveloping, captures in 129.5 / 320.1 ms on the
X100VI. The first Historical Original after a white-balance change still redevelops, since the slot
then holds the development before that change.

Memory is the edit launch's process RSS, sampled about every 50 ms by `ps` from the second pair's
captured frame to the process's end (the 100% and Fit steps), with the current entry's development
and the Original's both held after; the figure is each launch's maximum over that window, p50 / p95
over the same launches. GPU resources, captures and allocator retention are included, not
separated.

| Source (development planes) | Peak RSS p50 / p95 |
| --- | ---: |
| X100VI, full sensor (468 MiB) | 2290 / 2352 MiB |
| Z6, 6064 × 4040 sensor (280 MiB) | 1527 / 1559 MiB |
| Air 2S, 5568 × 3648 sensor (232 MiB) | 1450 / 1484 MiB |

The retained development adds its planes to the process. A development past the 600 MiB budget,
such as one at the 128 MP admission limit (about 1.43 GiB of planes), is never retained, so such a
photograph keeps one development and redevelops on every switch. At a redevelopment's
peak the process holds the retained development beside the one being built, so the X100VI's
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
per block rather than once per tap.
Complete-buffer tests compare the rows with the point evaluator for Basic, Mixer and their combined
recipe, a viewed source, a partial final chunk, colour
after a straightened crop at 4 and -30 degrees and after Presence, masked colour and a masked
Presence layer before a crop, on a stage narrower than one tap block and on one several blocks
wide.

The measurements below batch a source-only segment, in eight-row chunks; they are the evidence for
that path. Release core render on retained real RAW planes, 30 observations per recipe. Timers
include output allocation and drop, but exclude file read, decode/development, desktop scheduling,
GPU upload and presentation. Process CPU is cumulative over the shared 14-core Rayon pool, sampled
outside each render timer. No build or test ran with the timed profiles. Leg-start load was 6.38
for the Z6 and 5.39 and 5.80 for the two Fuji passes; the table gives the repeat Fuji pass.

| Camera / recipe | Wall p50 / p95 | CPU p50 / p95 |
| --- | ---: | ---: |
| Z6 / Full Basic | 146.0 / 155.2 ms | 1888.0 / 1949.4 ms |
| Z6 / Mixer | 167.9 / 175.8 ms | 2154.4 / 2190.5 ms |
| X100VI / Full Basic | 220.2 / 290.1 ms | 2598.8 / 2738.2 ms |
| X100VI / Mixer | 278.0 / 315.2 ms | 3127.7 / 3236.8 ms |

These are separate recipe workloads; do not sum them into a combined-stack estimate. Source planes
are 294 MB and 491 MB, outputs 97 MB and 159 MB. Row scratch is at most 676 KB and 1.30 MB across
14 folders. Activity Monitor footprint after decode was 556 MB and 925–928 MB, and about 752 MB and
1.244–1.247 GB with the render. Those snapshots are not allocator traces or GPU allocation
measurements.

The shared-pool Fit proxy check uses a 1920 × 1280 nearest-sampled proxy from actual Fuji retained
RAW planes and the Full Basic recipe. Thirty proxy requests are measured while one external caller
repeats exact full-source renders; proxy creation, RAW decode, build, desktop work, upload and
presentation are excluded. The exact worker completed 29 full renders. The process memory
footprint after overlap was 1.166 GB (resident 1.175 GB). Process CPU over the whole overlap window
includes both the exact worker and proxy requests.

| Exact render sharing the pool | Proxy alone p50 / p95 | Proxy with exact work p50 / p95 | Overlap wall / process CPU |
| --- | ---: | ---: | ---: |
| Production row batching | 12.1 / 13.1 ms | 207.2 / 214.7 ms | 6.0 s / 79.9 CPU s |

The contended proxy p95 of 215 ms misses the 32 ms acceptable input-to-presented-frame limit. A
one-row callback does not help: it measured 213.5/226.2 ms p50/p95 and 30 exact renders in 6.4
seconds, slightly behind the eight-row path. This measures a continuously active core exact render
and a core proxy, not a user's UI gesture or GPU presentation. The external load at the start was
below 8; no build or test ran alongside. Full scope and scratch accounting are in
[further performance opportunities](../research/further-performance.md#raw-colour-row-implementation-and-measurement).

### Shared-pool contention and rendering controls

One retained Fuji development competes with four full-Basic renders of a prepared 1920 × 1280
JPEG proxy. External callers start at a barrier and share Rayon. Each operation clock excludes
preparation, thread creation, hashing and frame drop; the proxy is hashed between requests while
RAW work continues. This is a CPU scheduling diagnostic, not desktop/GPU/presentation latency.
There are 30 development and 120 proxy observations; complete proxy hashes match. Leg-start load
was 2.1–5.2.

| Concurrent operation | p50 / p95 |
| --- | --- |
| Fuji retained-mosaic development | 367.8 / 379.7 ms |
| Full-Basic JPEG proxy render | 11.66 / 14.01 ms |

Schedules in which a preview waiter can steal long native work do not hold the proxy: draining row
groups gave 216 ms proxy p95, one row per callback still about 64 ms, and a four-worker cap still
about 60 ms. Ordinary pool callbacks contain at most eight tiles. The dependent final two rows run
on the external source caller, which holds nothing across a join.

A separate phase sweep starts preview work 0/75/125/175/225/275/325 ms after the RAW barrier,
five trials at each offset and four renders per trial. The seven 20-observation proxy p95 values
range from 12.3 to 17.6 ms; the maximum of all 140 renders is 27.0 ms. This diagnostic checks
later overlap as well as the ordinary early-start case; it is not a 30-sample distribution per
phase or a hard latency guarantee. Load was 2.8–3.4. Nested Rayon callers retain correctness and
liveness coverage, but the latency result applies to the production external source caller.

The official JPEG `editor-performance` diagnostic, with its composed geometry/crop and full Basic
recipe, gives the following 30-observation results. Source-preservation checks pass. Leg-start load
was 3.9–14.2; small shifts are not used as absolute latency-budget evidence.

| Workload | p50 / p95 |
| --- | --- |
| 24 MP full Basic + geometry | 78.2 / 81.2 ms |
| 60 MP full Basic + geometry | 182.5 / 188.9 ms |
| 24 MP source, full Basic proxy + geometry | 47.2 / 50.0 ms |
| 60 MP source, full Basic proxy + geometry | 49.3 / 53.1 ms |

### Qualification and remaining opportunities

Complete native float references, original hashes and sample/render/history checks pass on the
RAW open/edit/reopen journeys, and Fuji and DJI reopened captures are visually inspected alongside
their correlated state and events. Capture readiness waits for permitted current-bounds refits,
preserves intentional draft deferral and render-error evidence, and retries readbacks superseded by
newer photo pixels before publishing or saving them, so a screenshot from the temporary 1× proxy
cannot stand for the 2× refit. No sanitizer run, Windows/Linux numerical qualification or
cross-platform native GPU qualification is claimed by this M4 evidence.

The remaining candidates are ranked in [further performance opportunities](../research/further-performance.md).
Mosaic normalization on the development executor and RAW colour row batching are in production and
measured on actual owner inputs; their core shared-pool results are separate from presentation and do not replace end-to-end
evidence. RCD runs bounded tile jobs; its quiet-host timing and preview contention remain to be
measured. Startup attribution, GPU texture transfer, GPU
execution and SIMD/assembly remain open; CPU RGBA publication writes directly into the
frame owner, and no additional savings are assigned to it.

### JPEG export

Release `luxforge-json` on the owner's M4 Pro, warm file cache, one-minute load 2.8, driven over its line-delimited JSON by a script: `catalog.import`, the preparation job, one `edit.set-basic` committing `exposure: 0.3`, then five exports of that entry back to back, each timed from sending `export.write` to `job.read` first answering `ready`, polled every 20 ms. Five samples give a median and a maximum, not a p95. No RAW export needed a preparation: the committed edit had already developed the entry's settings. The encoder is libjpeg-turbo through `mozjpeg`; the encoder alone takes 48 ms at 24 MP and 121 ms at 60 MP ([export design](../design/export.md#decisions)).

| Source | Export wall p50 / max of 5 (ms) | Output | Dimensions |
| --- | ---: | ---: | --- |
| 24 MP JPEG (generated) | 90 / 110 | 688 KiB | 6000 × 4000 |
| 60 MP JPEG (generated) | 170 / 175 | 1.63 MiB | 10000 × 6000 |
| Nikon Z6 NEF | 118 / 127 | 2.35 MiB | 4024 × 6048 |
| Fujifilm X100VI RAF | 185 / 193 | 4.36 MiB | 7728 × 5152 |
| DJI Air 2S DNG | 120 / 131 | 6.29 MiB | 5464 × 3640 |

The encoder streams 16 rows at a time into the file, so it adds no whole-frame buffer.

Every figure here is the reference renderer's export. The GPU export ([export](../design/export.md#behavior), stage 4 of [GPU-first](../design/gpu-first.md)), whose tiles the desktop's tile worker draws, is timed against the reference export at 24 and 60 MP on a trivial and a heavy stack in [export](#export): on the heavy stack 0.28 s against 0.78 s at 24 MP and 0.63 s against 2.0 s at 60 MP. Both renderers' peak memory, and their export of the Z 6, are measured for every format and preset in [export settings](#export-settings).

### JPEG decode

Import decodes JPEG with libjpeg-turbo, through the same codec (`luxforge-jpeg`) as export. Decode only, on the owner's M4 Pro, release test build, warm file cache, the file read once outside the clock and neither hashing nor capture metadata timed: `source::jpeg_tests::decode_timing`, the median of 7 after one warm-up. The host was shared (one-minute load 16 to 18), so the figures are indicative.

| Source | Decode (ms) | Peak added | Dimensions |
| --- | ---: | ---: | --- |
| 24 MP JPEG (generated) | 22.7 | 92 MiB | 6000 × 4000 |
| 60 MP JPEG (generated) | 55.2 to 55.3 | 230 MiB | 10000 × 6000 |
| Drone photo, 10 MB | 75.0 to 75.5 | 55 MiB | 3389 × 4236 |

Peak added is `/usr/bin/time -l`'s maximum resident set size of the test process decoding the file once (`LUXFORGE_JPEG_ONLY=decode`), less the same process decoding nothing (`LUXFORGE_JPEG_ONLY=none`). It is the RGBA frame itself (92, 229 and 55 MiB) plus libjpeg's working buffers, under 1 MiB: rows go straight into the frame, or through a 16-row strip when the EXIF orientation turns the image. All three files are baseline with orientation 1; a progressive file adds libjpeg's coefficient buffer, 2 bytes per sample. x86_64 builds libjpeg-turbo's portable C path and is not measured.

### Native masking interaction qualification

The [masking interaction repairs](../design/masking-interactions.md) are measured on native Apple
M4 Pro/Metal, release, a 2880 × 1800 physical window at 2× scale, using background-only hidden
launches and isolated catalogs. Sources are the generated 6000 × 4000 and 10000 × 6000 JPEGs,
whose hashes stay unchanged. Filesystem reads are warmed by hash
verification; source preparation occurs at fresh-process open and is settled/warm during the
measured hover or stroke. These measurements do not describe hovering before a photo is loaded.
All runs start below the load-8.0 threshold; maximum start load is 7.55.

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

Two unbound-brush runs at 24/60 MP accept 30 positions each at 24 ms intervals, expose 29 live
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

The 1000-position Fit stroke fails the 4096-event diagnostics ceiling (3979 events
dropped); its final/late measurements are unavailable, and this is not a passing stroke result.
The failure remains in the evidence.

The aggregate report is `/private/tmp/mask-final-20260930/summary.json`, including every per-run
report path, quantile/sample count, load, RSS, counters and retained failure. Paced measurements use
`editor-latency --binary PATH --source fixtures/generated/24mp.jpg --output NEW_DIR --mode hover
--samples 30` and the corresponding 60 MP source, with `--zoom`, `--crop`, `--basic` or
`--action set-basic --parameter exposure` for the stated rows. Bound painting uses `--mode paint
--mask-overlay --samples 240` or `400`, with `--zoom 100` for the 60 MP viewport. Custom Off/identity,
unbound creation and sequential cursor proof scripts use `develop --background`; their exact scripts
and commands are retained alongside the reports.

The quick tier and all 36 rendered scenarios pass; `mask-interactions` contributes 44
correlated frames and independent coverage probes. Separate supplied Z6 NEF, X100VI RAF and Air 2S
DNG checks pass 30 captures and seven exact white-balance redevelopments each, with unchanged source
hashes, correct history and zero black-mask probe drift. That establishes functional RAW liveness,
not RAW latency qualification. Native Windows/Linux, GPU completion/scanout latency, the total
memory budget and the failed 1000-position diagnostic remain unqualified. No full-tier or broader
editor milestone is claimed.

## RawSpeed unpacking

For 64 catalog modes RawSpeed fills the sensor mosaic inside LibRaw's unpack
([RawSpeed unpacking](../design/rawspeed-unpack.md)). A mode is routed only when its mosaic is
exact and its unpack is at least 1.3× faster through RawSpeed; every routed mode meets both.

Native M4 Pro, 14 cores, 48 GiB, macOS 26.5.2, Rust 1.94.0, release with locked pins, on a host
shared with other sessions. Every p50 and p95 is nearest-rank, with no tails removed. Observations,
per-run load and scripts are retained locally under the measuring worktree's `target/task-007/`.

### Adapter unpack per routed mode

`rawspeed_unpack_timing` times `RawSource::decode` (identify, classification, unpack,
interpretation and the copy of the mosaic out of LibRaw, with the encoded bytes already in memory)
on every local authentic sample of every routed mode: 67 samples, one process. Reading the file and
dropping the decoded source are outside the clock and nothing is developed. Each sample is decoded
once by each unpacker first (the first routed unpack parses RawSpeed's camera data, about 4 ms) and
the two mosaics are compared; then 16 observations per unpacker in LibRaw, RawSpeed, RawSpeed,
LibRaw blocks. One-minute load was 2.6 to 4.2 per sample.

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
The lowest ratio is the Panasonic S5's 1.50×, outside the 1.2–1.4× band that is
rerun with more observations; every other mode is 1.84× or more. The spread within a leg is small:
the largest p95 over p50 is 12% (the Leica CL, Q2 and SL2 packed DNGs through RawSpeed, for example Q2 47.0 / 52.2 ms). The uncompressed DJI
DNGs take 8 to 13 ms either way, so their 2.2× saves under 7 ms. The owner Z6 unpacks in
203.6 / 207.4 ms with LibRaw and 65.5 / 66.5 ms with RawSpeed.

### Cold saved-white-balance preparation, LibRaw and RawSpeed

`cold_saved_white_balance_preparation_timing` in `luxforge-core` follows the method of
[native development and saved-white-balance preparation](#native-development-and-saved-white-balance-preparation):
a catalog whose RAW has a saved custom red gain of 1.1 × as-shot, a new owner and empty source cache
per observation with the filesystem warm, and the clock from immediately before the photograph's
`source.prepare` (taken through `catalog.import` of its file, which queues the same preparation)
until a strict exact-source `PreviewJob` for the current entry is available, through the one source
job's wait and adoption. The LibRaw leg builds the same source with the Z6's `NikonZ6Lossless14`
and the Air 2S's `DjiAir2sDng16` on LibRaw (their two `unpacker` lines removed); the RawSpeed leg
is the source as routed. The two builds differ only in those catalog lines, so each builds its own
catalog. 15 observations per leg in LibRaw, RawSpeed, RawSpeed, LibRaw order give 30 per variant.
The planes of every observation hash the same within and across the two builds.

| Source | LibRaw p50 / p95 | RawSpeed p50 / p95 | Median time saved | Leg load |
| --- | ---: | ---: | ---: | ---: |
| Nikon Z6 | 303.1 / 305.9 ms | 162.1 / 167.2 ms | 141.0 ms, 46.5% | 4.97–6.65 |
| DJI Air 2S | 205.2 / 207.2 ms | 198.9 / 204.6 ms | 6.3 ms, 3.1% | 3.89–4.97 |
| Fujifilm X100VI, not routed (control) | 364.3 / 368.3 ms | 364.7 / 366.9 ms | none | 3.73–4.75 |

The Z6 saving is the unpack saving above (138 ms), and the X100VI control, whose uncompressed mode
is the same in both builds, moves by 0.4 ms. The Air 2S's packed DNG unpacks in 13 ms either way, so
routing it saves little.

### Peak process RSS

One decode per process under `/usr/bin/time -l` (`one_decode_for_peak_rss`), LibRaw forced and
RawSpeed, five alternating processes each, on the largest routed sensors: the Nikon D850 lossless
NEF (1840, 8288 × 5520, a 91.5 MB mosaic) and the Leica Q2 packed DNG (3204, 8424 × 5632,
94.9 MB). Every run of a variant read within 0.1 MB of the others; the table gives each variant's median.

| Sample (encoded size) | LibRaw peak RSS | RawSpeed peak RSS | Difference |
| --- | ---: | ---: | ---: |
| D850 1840 (55.2 MB) | 242.6 MB | 248.6 MB | +6.0 MB (5.7 MiB) |
| Q2 3204 (87.2 MB) | 281.4 MB | 287.2 MB | +5.9 MB (5.6 MiB) |

The design allows for a transient second mosaic of about 91 MiB. It does not raise the peak: the
peak is the encoded bytes, LibRaw's raw buffer and the mosaic the adapter keeps, and RawSpeed's
image is released before the adapter allocates that mosaic. What remains is RawSpeed's code and
parsed camera data.

### What these measurements do not claim

The unpack figures are the adapter's decode alone, not an editor open; a first open also reads and
hashes the file, develops, converts and renders. The cold-preparation figures exclude owner
startup, catalog open, rendering and presentation, and cover only the two owner originals that are
routed. RSS is one decode in a test process, not the editor's peak, which also holds developments
and GPU resources. The `timing` tier (`editor-performance`, `editor-latency` and `measure`) opens
generated JPEGs only and does not exercise RAW unpacking. Native Windows and Linux builds are not
measured.

## Catalog: browse, pick, develop

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, release builds, `cargo run --release --locked --package xtask -- catalog-measure --scale full --raw-corpus <the private CC0 corpus>` (30 samples a figure, 5 a first-browse journey), on a host shared with other sessions: one-minute load 13 to 18 throughout, above the harness's threshold of 8, so every figure is marked unreliable by the harness and none is a baseline. The data is generated in scratch: a 1,000-frame RAW trip copied from 102 CC0 files of the corpus under new names with rewritten capture times (the originals read only, checked unchanged), a 10,000-file folder and a 200,000-file tree of APFS clones of generated JPEGs, a 200,000-link tree of hard links, and a generated 100,000-photograph catalog. The OS file cache is warm; no card reader was measured. The photographs-view rows come from the harness's `--only browse-generated` step, four runs at load 10 to 23.

| Target (provisional, [catalog design](../design/catalog.md#performance)) | Measured | Verdict |
| --- | --- | --- |
| First browse of a 1,000-frame folder, internal SSD: first grid screen within 1 s; every file, event and moment within 5 s | first screen 181 / 246 ms p50 / p95; every file, event and moment 182 / 249 ms | Met |
| Grid previews for every file of that first browse: reported | 11.7 / 15.1 s (embedded previews of 1,000 RAWs read on two workers; the Canon R5 Mark II and R8 developed) | Reported |
| Browsing from a card reader: reported | Not measured (no card) | Outstanding |
| Returning to a known 1,000-file folder: reconciled and drawn within 1 s | reconciled 4.4 / 4.8 ms, drawn 12.1 / 12.6 ms | Met |
| First index of 200,000 files: reported, the editor responsive | 55.0 s; owner round trips during it 0.025 / 0.41 ms. 200,000 hard links to 500 files: 54.4 s, 0.026 / 0.046 ms | Reported; responsive |
| `browse.view` p95 under 50 ms: 10,000 files | 18.4 / 27.5 ms (first evaluation 66 ms) | Met |
| `browse.view` p95 under 50 ms: 100,000 photographs | 106 / 107 ms at best, 119–167 / 169–188 ms in the other runs, first evaluation about 0.9–1.0 s | **Missed.** Stepping the photographs' rows in SQLite alone takes about 52 ms; the two ways past it are a [proposal](../design/catalog.md#performance) |
| `browse.rows` of 200 under 2 ms | files 2.5 / 3.9 ms, photographs 2.7 / 4.1 ms (0.9 / 1.2 and 1.1 / 1.4 ms in a run at load 6 to 17) | Missed under this load; met at load 6 to 17 |
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

The CPU approximations the GPU programs stand in for, measured with the [GPU preview error measure](../design/gpu-preview.md#the-preview-error-limit) so the owner can read the proposed limits against them: the Presence proxy against the exact downscale at Fit and a RAW white-balance draft against its release. The GPU draws these drags on a machine with one ([below](#a-raw-white-balance-drag-on-the-gpu-against-its-release), [the picture in motion](#the-picture-in-motion)). It is context, not a gate, and it changes no limit. The proposed limits are, per statistic (mean ΔE00, worst 16 × 16 block, p99 ΔE00, signed mean ΔL\*): pointwise 0.5, 1.0, 2.0, ±0.25 and spatial 1.0, 2.5, 5.0, ±0.5.

### Scope

- **Measure.** `cargo xtask preview-error --evidence DIR --candidate-frame N --reference-frame M` (CIEDE2000 in `f64` from 8-bit sRGB through linear light, XYZ and CIELAB under D65, `luxforge_reference::preview_error`) over the photograph rectangle the editor records for each frame, clipped to the canvas, so the canvas beside the photograph never counts. Candidate is the approximate frame, reference the frame that replaces it, and the signed ΔL\* is candidate minus reference. An independent NumPy implementation of the same formulas agreed to six decimals on all four statistics for three of the frame pairs (`presence-24mp` Dehaze, Air 2S Fit white balance, X100VI all three Presence fields); that check is not committed. The report also carries each frame's own recorded state (status label, `render_proxy`, `reduced`, `drawn_region_quality`), which confirms each pair below is the frames it names.
- **Host and build.** Apple M4 Pro (14 cores), macOS 26.5.2, Metal on the `Apple M4 Pro` adapter, a hidden 1440 × 900 logical window at 2×, so 2880 × 1800 physical captures, from a release build of `luxforge-app`. Every launch was a hidden window in the background-only bundle. The host was shared (one-minute load 13 to 29 during the launches); these are pixel measurements, not timings, and a loaded host does not change them.
- **Sources.** The generated 24 MP and 60 MP JPEGs (four flat colour quadrants, corner labels, an arrow and fine vertical detail, so they hold almost nothing for Presence to act on) and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest (`nikon-z6`, `fujifilm-x100vi`, `dji-air2s`; hashes in [`fixtures/preview/corpus.json`](../../fixtures/preview/corpus.json)). Fit photograph rectangles are 1716 × 1144 (X100VI, Air 2S, 24 MP), 1716 × 1030 (60 MP) and 1004 × 1508 (the Z6, which opens portrait); at 100% it is the whole 1796 × 1660 canvas, the photograph's centre with no pan.
- **One run each.** Each cell is one launch and one drag value, with deterministic pixels but no distribution over photographs, values or pans. Both frames of a pair come from the same launch.

### Presence proxy against the exact downscale at Fit

Candidate: the frame drawn while a Presence slider is dragged to +100 and held, status "Approximate render", the display-bounded proxy. Reference: the frame after the slider is released, status "Exact render" with `settled_from_exact`, the linear-light area reduction of the exact full-resolution render. The measured CPU path displays an exact-derived Fit frame only for a stack that holds a non-neutral Detail layer (Detail declares `fit_settle: exact`; Presence alone keeps its proxy as the displayed Fit frame), so every stack below also holds Detail's Colour noise suppression at 1, committed first. The control rows are that Detail layer alone, dragged and released: what settling moves with no Presence at all. It moves nothing on the generated JPEGs and, on the RAWs, the RAW proxy's own difference from the exact reduction (mean 0.41 on the Z6, 0.08 on the X100VI, 0.97 on the Air 2S), which a Presence row includes and cannot be subtracted from. The stack in the last row of each source is Texture and Clarity committed at +100 with Dehaze dragged to +100.

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

### RAW white-balance draft against its release

The `raw-panel` smoke scenario's frames: a Temperature drag left open at 3500 K at Fit, and again at 2500 K at 100%, each against its release at the same value, which redevelops the mosaic. The draft is the approximation `W = R · diag(g'/g) · R⁻¹` on the planes developed at the committed white balance; the release is the exact development. At Fit the pair is the draft (frame 2) and the release's proxy (frame 3). At 100% the held draft after full-detail refinement (frame 6) is held against the release's exact region (frame 7).

| RAW white balance, Temperature drag | Mean ΔE00 | Worst 16 × 16 block | p99 ΔE00 | Signed mean ΔL\* |
| --- | ---: | ---: | ---: | ---: |
| Z6 NEF, Fit: draft against release | 0.038 | 0.38 | 0.74 | -0.000 |
| Z6 NEF, 100%: held refined draft against release | 0.062 | 0.44 | 0.48 | -0.000 |
| X100VI RAF, Fit: draft against release | 0.059 | 0.24 | 0.66 | +0.000 |
| X100VI RAF, 100%: held refined draft against release | 0.142 | 0.45 | 0.73 | -0.003 |
| Air 2S DNG, Fit: draft against release | 0.313 | 2.68 | 1.84 | -0.030 |
| Air 2S DNG, 100%: held refined draft against release | 0.408 | 3.11 | 3.08 | -0.068 |

Against the proposed pointwise limits, the Z6 and X100VI drafts are within them at Fit and when held at 100%, and the Air 2S misses the worst block at Fit (2.68) and the worst block and p99 when held at 100%.

### A RAW white-balance drag on the GPU against its release

The GPU draws a Temperature drag over the planes the entry developed, the change `W` a leading step ([GPU previews](../design/gpu-preview.md#raw-white-balance)), and the owner's `raw-panel` gates hold that moving frame to the release's picture at rest within the pointwise limits, at Fit and over the visible region at 100%. Measured by `gpu_white_balance_on_the_raw_corpus` (`crates/luxforge-app/src/app/gpu_white_balance_tests.rs`, ignored; the private RAW manifest), on the M4 Pro's Metal adapter: each RAW opened As shot, a `set-raw` draft at 3500 K and at 2500 K planned at Fit (the harness's Fit bounds) and over the largest view's centred region at 100%, drawn by the editor's surface over its boundary derived from the As shot source; then the same temperature committed, redeveloped, and its picture at rest drawn from the redeveloped source (its tiles reduced to the view at Fit, its region plan at 100%) and its view plan, the frame a drag over the redeveloped source draws. Figures are mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / signed mean ΔL\*; one draw each, deterministic.

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

The owner's relative-residual gates (held 100% residuals of 0.8%, 1.4% and 5.1%, [decisions](../decisions.md#raw-white-balance-drafts)) are not remeasured here.

### Reproducing it

The Presence journey is a source-independent evidence script, [`fixtures/preview/baseline/presence-fit.json`](../../fixtures/preview/baseline/presence-fit.json). Frame 1 is the open and each step is the frame after it. The Presence pairs (candidate, reference) are the Detail control (2, 3), Texture (4, 5), Clarity (8, 9), Dehaze (12, 13) and all three (17, 18). The white-balance pairs come from the `raw-panel` smoke scenario's evidence: Fit (2, 3) and 100% held (6, 7).

The Presence journey draws the GPU's frames, not the CPU proxy, so it does not reproduce these pairs:

```sh
cargo xtask build --release
cargo xtask develop --background --hidden-window --evidence-dir /tmp/NEW_DIR \
  --evidence-script fixtures/preview/baseline/presence-fit.json --open SOURCE --window-size 1440 900
cargo xtask preview-error --evidence /tmp/NEW_DIR --candidate-frame 4 --reference-frame 5
```

The comparison under the current contract is the release gate's picture in motion, a drag's frame against the picture at rest it settles to and against the reference renderer's frame at the view, not against a CPU frame (a selection, so the run reports itself incomplete):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind picture-in-motion --families presence --zoom fit
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --kind picture-in-motion --families basic --zoom 100
```

## GPU colour programs at Fit

The seven pointwise colour programs ([GPU previews](../design/gpu-preview.md#qualifying-a-program)) and the geometry tail ([GPU previews](../design/gpu-preview.md#where-the-code-lives)) against the CPU frame the preview worker's proxy drew at Fit, on every colour and geometry recipe of the [corpus](../../fixtures/preview/corpus.json) (`basic`, `tone-curve`, `mixer`, `vignette` and the four together, `colour-stack`; a 16:9 crop straightened by 7° and by 45° and a tight crop straightened by 7°, a Perspective warp and, on the RAWs and on the zone plate with a lens identity, their lens profile with it, each under a Basic edit) over every source this host has. Pixel measurements, not timings. The CPU proxy is not part of the editor, so these figures qualify the programs, not the editor's frames.

### Scope

- **Measure.** `cargo xtask preview-error --candidate GPU.png --reference CPU.png --photo-rect 0,0,W,H --class pointwise` over each pair; all 62 measured pairs pass. Candidate is the GPU frame, reference the CPU frame; the photograph is the whole frame.
- **The frames.** The CPU frame was the desktop's own Fit job (`ready_preview_job` with the Fit bounds) rendered by the preview worker's proxy phase, or, for a photograph that fits the bounds at its own size, its exact phase. The GPU frame was the same stack planned with `gpu_plan` over the proxy the worker built — a window of the proxy stage when the stack reads less than all of it, as a crop does, held at its origin — as the boundary the worker's job writes, or for a frame at the exact stage the window of the source its whole output reads, as the worker renders it (`qualification::region_boundary` over the whole output, `Render::output_boundary`) (`rgba16float` on a JPEG, `rgba32float` on a RAW), converted by `app::gpu_plan` with a lens warp's coordinate grid over the output stage (a perspective's homography needs none) and drawn by the photo surface's own assembled shader, whose last pass encodes as the CPU's output quantizer does, read back headlessly. Both are compared at the stage's own size, before the surface's placement draws them on screen; the [readback test](../design/gpu-preview.md#where-the-code-lives) shows the two draw identically for identical codes.
- **Bounds.** 1716 × 1508 physical pixels: an evidence run's 1440 × 900 logical window at 2× with both panels open, as `proxy_bounds_for` computes them.
- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The `test` profile build of `luxforge-app`, `gpu_colour_corpus_at_fit`. One run; the pixels are deterministic.
- **Sources.** The generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture (matching the corpus) and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest. The Presence fixture (1440 × 960) fits the bounds, so its Fit frame is the exact render.
- **RAW lens profiles.** The Z6 and Air 2S commit their lens profile at first open, a warp after the colour layers, so each of their cells draws the geometry tail through the profile's coordinate grid; the zone plate with a lens identity has its detected profile applied as the Lens section's Apply applies it. The vignette alone on those two is a gap: a finishing layer after a warp receives the warped frame, which only the worker's boundary job holds, and the harness holds the proxy source. The X100VI commits no profile.
- **Straightened crops.** A crop behind the Z6's and Air 2S's lens profile is fused into the warp's resample, and its Fit frame was a windowed proxy as on every other source, whose boundary holds only the window of the proxy stage the crop reads: at 7° 1781 × 1143 and 1819 × 1171 `f32` texels (32.6 and 34.1 MB) of 1821 × 2738 and 1821 × 1213, and at 45° 1869 × 1869 and 1899 × 1896 (55.9 and 57.6 MB), each slot charging 65.7 to 103.2 MB. The tight crop's output fits the bounds on every source but the 60 MP JPEG, so its Fit frame is the exact phase's, over the window of the source its output reads: 969 × 1343 texels of the Z6's 4024 × 6048 (20.8 MB, where the whole source is 389 MB), 1173 × 998 of the Air 2S's 5464 × 3640 (18.7 of 318 MB) and 1636 × 1389 of the X100VI's 7728 × 5152 (36.4 of 637 MB), each slot 44.9 to 71.3 MB.

### Results

Mean ΔE00, worst 16 × 16 block, p99, signed mean ΔL\* and largest ΔE00 of the drawn frame, and what the slot drawing it charges the GPU-preview budget. The drawn frame and the program's `f32` output through the reference quantizer are the same in every cell: the last pass encodes as the CPU's quantizer does, where the hardware's sRGB encoder rounds some values to the neighbouring code (putting the RAW cells' means at 0.04 to 0.12 against 0.01 to 0.04 for the program output).

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

Every measured cell is within the pointwise limits (mean 0.5, worst block 1.0, p99 2.0, ΔL\* ±0.25): the largest figures are a mean of 0.051, a worst block of 0.25, a p99 of 0.87 and a ΔL\* of −0.002. Without a lens warp the GPU frame is the CPU's to within a fraction of a code: the JPEGs, whose boundary is each 8-bit code's linear value held as a half float, and the X100VI, whose `f32` boundary holds what the CPU reads. A perspective warp's tail is its homography, evaluated at every pixel, so it keeps that: over the zone plate, whose chirp reaches half a cycle a pixel, its worst block is 0.04. The Z6 and Air 2S cells and the zone plate with a lens identity carry a lens profile's warp, which the tail samples through its coordinate grid, within 0.025 px of the exact map the CPU samples through; their means are 0.01 to 0.05 and their worst blocks 0.07 to 0.20, the largest the Air 2S's crop at 45°. The RAW tail's `rgba32float` intermediate keeps the linear values unquantized, and the RAW cells carry a signed ΔL\* within ±0.0001, where an `rgba16float` one written by the M4's own conversion toward zero carries −0.004 to −0.010 ([plane precision](#plane-precision)). A windowed boundary changes nothing the tail draws: on the device, the frame over the window a crop reads is the whole boundary's, bit for bit, through an affine, a projective and a lens tail, and with Texture and Clarity before the tail over the window grown by their margin (`cargo test -p luxforge-app gpu_window`).

### Reproducing it

The harness that measured these figures, `gpu_colour_corpus_at_fit`, compared against the CPU proxy and is not in the repository. The release gate runs the same families at Fit against the reference renderer's frame reduced to the view, not against the CPU proxy, so it reproduces the comparison under the current contract, not these figures. The picture in motion is the drag's frame these figures measured, held against the picture at rest and the reference; the picture at rest is what the gate judges. A selection, so the run reports itself incomplete:

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families basic,tone-curve,mixer,vignette,colour-stack,crop,lens-perspective
```

## GPU mask coverage at Fit

The coverage programs of every mask kind ([GPU previews](../design/gpu-preview.md#mask-coverage)), each carrying the corpus's masked Basic layer (Exposure +0.8, Contrast 20, Vibrance 30), against the CPU frame the preview worker's proxy drew at Fit, on every masked recipe of the [corpus](../../fixtures/preview/corpus.json) (`mask-linear`, `mask-radial`, `mask-brush`, `mask-luminance-range`, `mask-colour-range` and the algebra, `mask-composed`, a linear less a radial) over every source this host has. These are the figures the programs are enabled on. Pixel measurements, not timings.

### Scope

- **Measure.** `cargo xtask preview-error --candidate GPU.png --reference CPU.png --photo-rect 0,0,W,H --class pointwise` over each pair; all 42 measured pairs pass, the zone plate's six reported in [the qualification](#the-corpus-error-report).
- **The frames.** As for [the colour programs](#gpu-colour-programs-at-fit): the desktop's own Fit job through the preview worker against the same stack planned with `gpu_plan` over the same proxy source held as the worker's boundary holds it, converted by `app::gpu_plan` into one masked step, and the lens profile's tail on the Z6 and Air 2S, and drawn by the photo surface's own assembled shader, read back headlessly. The corpus names its mask and components by name; the harness resolves each to its identity, and plans from the recipe the frame was rendered from, whose painted strokes are resolved.
- **Bounds, host and build.** 1716 × 1508 physical pixels. Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The `test` profile build of `luxforge-app`, `gpu_mask_corpus_at_fit`. One run; the pixels are deterministic.
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

The harness that measured these figures, `gpu_mask_corpus_at_fit`, compared against the CPU proxy and is not in the repository. The release gate runs the mask families at Fit against the reference renderer's frame, not the CPU proxy, so it reproduces the comparison under the current contract, not these figures, as [for the colour programs](#gpu-colour-programs-at-fit):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families mask-linear,mask-radial,mask-brush,mask-luminance-range,mask-colour-range,mask-composed
```

## GPU Presence program

The Presence program ([GPU previews](../design/gpu-preview.md#spatial-programs)) on the M4, against the CPU per filter, per unit and on the [corpus](../../fixtures/preview/corpus.json)'s Presence recipes at Fit. Pixel and arithmetic measurements, not timings.

### Scope

- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal (`wgpu-hal` 27.0.4, Metal's default fast math). The `test` profile build of `luxforge-app`. One run each; the pixels are deterministic.
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

`gpu_presence_corpus_at_fit`, the shared harness the colour programs were qualified with ([GPU colour programs at Fit](#gpu-colour-programs-at-fit)): the desktop's Fit job for the CPU frame, the CPU proxy, the same stack planned from the Presence layer over the same proxy source for the GPU frame, the RAW cells planned for the linear path over an `rgba32float` boundary. The light is taken on the GPU (the harness holds no store of the worker's), which over a whole Fit stage selects the CPU's blocks. Each pair judged by `cargo xtask preview-error --class spatial`: all 49 measured pairs pass, the zone plate's seven reported in [the qualification](#the-corpus-error-report). Bounds 1716 × 1508.

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

The Z6 and Air 2S cells carry the lens profile their first open commits, which the geometry tail draws after the spatial step, and their means are mostly the tail's, as for the colour programs. Its `rgba32float` intermediate holds their signed ΔL\* within ±0.0001, where an `rgba16float` one written by the M4's own conversion toward zero held them at −0.004 to −0.010; the X100VI commits none, and its `f32` boundary leaves the GPU frame within a fraction of a code of the CPU's. The masked recipe's radial selects part of each frame: its masked CPU frame equals the unmasked one on 5 to 10% of the pixels, where coverage is whole. [The qualification](#the-corpus-error-report) measures the zone plate. The largest maxima are the Air 2S's near-black pixels ([below](#isolated-near-black-pixels)).

**Memory.** "Charged" is what the photo surface's slot drawing the plan charges the GPU-preview budget, the figure its `gpu_preview_in_use_bytes` reports (a surface test holds the two equal): the boundary, the output in the photograph's size bucket and its uniform, the words and blocks buffers, and the planes. With all three fields on the 60 MP JPEG at Fit (a 1716 × 1030 proxy), the planes take 50.5 MB: about 28.6 bytes a pixel, Texture's full-resolution planes 24 of them, of which its apply keeps only its one-channel band's 4 ([design](../design/gpu-preview.md#plane-sharing-and-precision)). In every row that holds Texture the table's charge includes a two-channel band plane, 4 bytes a texel of the boundary more than the one-channel band takes; with it this slot charges 88.5 MB (84.4 MiB) of the 2 GiB budget, its planes 57.6 MB. One slot is held at a time, so this is its peak; a replacement overlaps the slot it replaces until that retires. The 100% figures are in [GPU previews at 100%](#gpu-previews-at-100).

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

The near-black run writes `near-black.json`, every cell's figures for every variant at each scale; its Fit cell is the development reduced to the size the picture at rest's plan gives and drawn as a photograph of its own on both sides, where the figures above drew the proxy, so a run measures a different Fit frame. The corpus harness, `gpu_presence_corpus_at_fit`, compared against the CPU proxy and is not in the repository. The release gate runs the Presence family at Fit against the reference renderer's frame, not the CPU proxy, so it reproduces the comparison under the current contract, not these figures, as [for the colour programs](#gpu-colour-programs-at-fit):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families presence
```

## GPU preview compile cost

What compiling one program sequence costs the photo surface's compile thread ([GPU previews](../design/gpu-preview.md#where-the-code-lives)): the stage's own `compile` from the programs' `naga` checks to the driver's pipelines, the content pass's and a spatial step's, timed on the calling thread for each sequence a Fit drag of the colour modules draws. A functional measurement for the design's choice of one pipeline per sequence compiled off the interface thread, not a timing gate.

- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The `test` profile build of `luxforge-app`, `gpu_preview_compile_cost_per_sequence`, three runs in new processes.
- **Figures.** Basic (four units) 2.8 to 2.9 ms; the Tone curve 1.4 ms; the Mixer 2.0 ms; the Vignette 1.4 ms; Basic under a curve and a mixer (six steps) 3.8 to 3.9 ms; Basic under every colour module (seven) 4.2 to 4.3 ms. A second compile of the same sequence in the same process is within 0.4 ms of the first.
- **The driver's cache.** Metal keeps compiled shaders on disk across processes, so these are sequences it had seen. The one cold figure observed, the six-step sequence, took 97.4 ms; the others were already cached. A cold compile is what the compile thread and the warm list keep off the interface thread.

```sh
cargo test -p luxforge-app gpu_preview_compile_cost_per_sequence -- --ignored --nocapture
```

### Presence and Detail sequences in the editor

What a program sequence the `gpu-preview` scenario draws costs the editor's compile thread, from the queue to a kept pipeline, on a cold Metal shader cache (a background bundle identifier of its own, whose cache starts empty). Apple M4 Pro, release editor, one line printed per compile.

- **After a Presence commit.** The Dehaze commit warms six sequences, the Presence drag's own last, and on a cold cache that one is ready 5.9 to 6.4 s after the commit (7.3 s on a host at load 30 to 60). A fixed quiet after a commit covers that only on an idle host: under load a cold cache fails a drag's first GPU tick (`compiling`), so the scenario waits for the warm list to compile (`gpu_warmed` steps, [design](../design/gpu-preview.md#where-the-code-lives)), 6.3 to 6.6 s after the Presence commits and 8.5 to 8.6 s with twelve busy loops beside the editor. A link's own cold compile is in [compacted pass modules](#compacted-pass-modules).
- **At 100%** (`gpu-preview-zoom`) a percentage view's plans are not warmed, and each drag compiles its own when its boundary is first drawn: cold, a Presence drag's region sequence in 1.09 to 1.12 s, a Basic drag under Presence's in 0.57 s and a Detail drag with Presence after it (Dehaze behind Detail) in 1.85 to 1.90 s, warm in 23 to 52 ms. The scenario holds each of those drags after its first tick until that compile has ended, at least 4 s and at most 60 s.

### Warming at launch and open

The warm-up's own duration as the editor records it (`gpu_warm_up`, [GPU previews](../design/gpu-preview.md#warming-at-launch-and-open)): from a warm list handed to the idle compile thread until its queue drained, and when the newest list's open stack's part, with the frame's own sequences, had compiled. Release editor, the `gpu-preview` scenario on the M4 Pro (Metal): one run per row, functional figures, not a timing gate.

| Run | Shader cache | First warm-up | Open stack's part | Reference frame on screen before the GPU's |
| --- | --- | ---: | ---: | ---: |
| `LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=task009-cold-1`, a first launch of the build | A bundle identifier of its own, its Metal cache empty | 4,389 ms, 11 sequences over two lists (the release's list joined it) | 1,510 ms (the second list's) | not recorded |
| `task009-cold-2`, the next launch | Its own, empty | 1,525 ms, 6 sequences | 41 ms | 38 ms (`surface_frame_drawn` `cpu` at 957 ms, `gpu` at 995 ms) |
| The shared background bundle | Warm | 37 ms, 6 sequences | 2 ms | |

The second cold run is faster than the first although its bundle's cache was empty too: macOS also keeps a compiler cache outside the bundle's, which the first run filled and which was not cleared, so neither row is a whole-system cold cache. Later warm-ups in both cold runs, as each commit warmed its stack, took 0.3 to 0.7 s and compiled only what the stack added.

```sh
cargo build --release --locked -p luxforge-app
LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=NEW_SUFFIX cargo run --release --locked --package xtask -- smoke --scenario gpu-preview --output NEW_DIR
```

### Compacted pass modules

Every spatial pass's module reaches wgpu as the `naga` module the surface validated, compacted to its entry point ([GPU previews](../design/gpu-preview.md#where-the-code-lives)): `naga` leaves a module of one entry point uncompacted, so uncompacted the driver compiles the whole spatial program, every kernel and apply, for every pass. `gpu_preview_spatial_compile_cost_on_a_cold_cache` compiles, in one process and in the order the editor's warm lists do, the links of seven Presence and Detail sequences — a Presence drag's own, drags of colour layers under it and a Detail drag with Presence after it — which create 20 pass pipelines in all, the links sharing the rest. Every kernel and apply opens with a test of its words against the process's own constant, so the driver's shader cache holds none of them; a second run handed the first run's constant measures them warm. The headless `test` profile build of the chain of links, one binary compacting and one handing wgpu the WGSL text, alternated three times each on the `Apple M4 Pro` adapter (Metal), at a one-minute load of 2.5 to 2.7. Cold, milliseconds:

| Sequence | Compacted | WGSL text |
| --- | ---: | ---: |
| A Presence drag: Presence in its GPU shape, 22 passes from 14 pipelines | 910 / 941 / 951 | 1,197 / 1,180 / 1,174 |
| A Detail drag, Presence after it | 390 / 384 / 371 | 417 / 446 / 436 |
| All seven | 1,499 / 1,401 / 1,394 | 1,689 / 1,702 / 1,685 |

Compacting takes about a fifth off a Presence link's cold compile and 12 to 18% off the seven. It removes only what the entry point never reaches: over ten plans frames hash the same from the text, the uncompacted module and the compacted one, and every GPU test passes compacted.

```sh
cargo test -p luxforge-app gpu_preview_spatial_compile_cost_on_a_cold_cache -- --ignored --nocapture
LUXFORGE_COMPILE_CONSTANT=EARLIER_RUNS_CONSTANT cargo test -p luxforge-app gpu_preview_spatial_compile_cost_on_a_cold_cache -- --ignored --nocapture
```

## GPU Detail program

The Detail program ([GPU previews](../design/gpu-preview.md#spatial-programs)) on the M4, against the CPU per kernel, per unit and on the [corpus](../../fixtures/preview/corpus.json)'s Detail recipes at Fit. At Fit the GPU frame is judged against the CPU's moving proxy it stood in for ([decisions](../decisions.md#gpu-previews)), a frame the editor does not draw; the GPU draws Detail's picture at rest too. Pixel and arithmetic measurements, not timings.

### Scope

- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal (`wgpu-hal` 27.0.4, Metal's default fast math). The `test` profile build of `luxforge-app`, with half-precision planes written rounded to the nearest half ([plane precision](#plane-precision)). One run each; the pixels are deterministic.
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

An `rgba16float` RAW geometry-tail intermediate loses extended linear range, so the tail uses `rgba32float`; a synthetic native GPU identity-tail regression verifies bit-for-bit `f32` preservation, and the native RAW geometry test matches CPU output codes exactly. Over the corpus on the three RAWs, the colour, mask, Presence and Detail harnesses at Fit and every family at 100%, the `rgba32float` tail draws every RAW cell within the limits, its geometry cells within a signed ΔL\* of ±0.0001, and its intermediate charges each such slot its `f32` bytes ([GPU previews at 100%](#gpu-previews-at-100)); an in-editor memory measurement of a photo-sized RAW drag remains outstanding.

Which spatial planes are held at half precision ([design](../design/gpu-preview.md#plane-sharing-and-precision)), plane by plane: `gpu_detail_plane_precision_is_measured` draws each stack in the GPU shape a drag draws over the synthetic photograph at 1536 × 1024 on the linear path, with every plane at full precision, as shipped, and with each full-resolution plane alone at half precision (`rgba16float`), against the CPU's exact frame; the largest linear difference is against the full-precision program. One run on the M4.

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
- **Rounding.** Before each write to a half-precision spatial plane the surface rounds to the nearest half, ties to even. Without it the M4 converts a storage write toward zero: of 49,152 values in every binade a frame holds, of both signs, 24,614 were held toward zero and the rest at the nearest, which every one is with the rounding (`a_half_planes_texels_are_the_nearest_half`). Through noise reduction's four chained smoothings that drew the moderate stack darker by a signed ΔL\* of −0.12 to −0.13 (mean 0.26). An `rgba16float` RAW-tail intermediate also narrows the linear values and draws RAW geometry darker; the `rgba32float` tail preserves them without half rounding.

### The corpus at Fit

`gpu_detail_corpus_at_fit`, the shared harness, over the corpus's six single-layer Detail recipes (its three chained ones are [below](#after-detail-presence)): the study's moderate, noise-stress and sharpen-stress settings, each unmasked and through the radial mask, on every source this host has. The GPU frame was planned from the Detail layer over the same proxy source the worker rendered from, with the units compiled at the proxy's scale as the CPU's proxy compiled them, and drawn with the surface's own encoding, which is the CPU quantizer's; the RAW cells were planned for the linear path over an `f32` boundary, and the Z6 and Air 2S keep the lens profile their first open commits, which the geometry tail warps. Each cell was judged against the CPU's moving proxy. Each pair judged by `cargo xtask preview-error --class spatial`: all 36 exit 0. Bounds 1716 × 1508.

Mean, worst 16 × 16 block, p99 and signed mean ΔL\* of the drawn frame against the CPU's moving proxy:

| Recipe, source | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Charged |
| --- | ---: | ---: | ---: | ---: | ---: |
| Moderate, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.02 / 0.01 | 0.00 / 0.00 | +0.000 / −0.000 | 126.7 / 115.8 MB |
| Moderate, Presence fixture (exact) | 0.006 | 0.02 | 0.36 | +0.000 | 94.2 MB |
| Moderate, Z6 / X100VI / Air 2S | 0.023 / 0.011 / 0.025 | 0.09 / 0.08 / 0.12 | 0.73 / 0.52 / 0.74 | −0.000 / +0.000 / −0.000 | 137.9 / 142.4 / 173.8 MB |
| Noise stress, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.03 / 0.01 | 0.00 / 0.00 | −0.000 / −0.000 | 111.0 / 101.6 MB |
| Noise stress, Presence fixture (exact) | 0.001 | 0.02 | 0.00 | +0.000 | 83.1 MB |
| Noise stress, Z6 / X100VI / Air 2S | 0.022 / 0.011 / 0.022 | 0.09 / 0.07 / 0.12 | 0.71 / 0.52 / 0.70 | +0.000 / +0.000 / −0.000 | 125.7 / 126.7 / 158.1 MB |
| Sharpen stress, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.00 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 87.5 / 80.4 MB |
| Sharpen stress, Presence fixture (exact) | 0.000 | 0.00 | 0.00 | +0.000 | 66.5 MB |
| Sharpen stress, Z6 / X100VI / Air 2S | 0.023 / 0.000 / 0.027 | 0.10 / 0.01 / 0.14 | 0.74 / 0.00 / 0.76 | +0.000 / +0.000 / +0.000 | 107.6 / 103.2 / 134.6 MB |
| Moderate, masked, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.02 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 126.7 / 115.8 MB |
| Moderate, masked, Presence fixture (exact) | 0.000 | 0.02 | 0.00 | +0.000 | 94.2 MB |
| Moderate, masked, Z6 / X100VI / Air 2S | 0.022 / 0.001 / 0.021 | 0.10 / 0.05 / 0.12 | 0.71 / 0.00 / 0.69 | −0.000 / +0.000 / +0.000 | 137.9 / 142.4 / 173.8 MB |
| Noise stress, masked, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.03 / 0.01 | 0.00 / 0.00 | −0.000 / +0.000 | 111.0 / 101.6 MB |
| Noise stress, masked, Presence fixture (exact) | 0.000 | 0.01 | 0.00 | +0.000 | 83.1 MB |
| Noise stress, masked, Z6 / X100VI / Air 2S | 0.021 / 0.002 / 0.021 | 0.09 / 0.05 / 0.12 | 0.70 / 0.00 / 0.68 | −0.000 / +0.000 / +0.000 | 125.7 / 126.7 / 158.1 MB |
| Sharpen stress, masked, 24 MP / 60 MP JPEG | 0.000 / 0.000 | 0.01 / 0.01 | 0.00 / 0.00 | +0.000 / +0.000 | 87.5 / 80.4 MB |
| Sharpen stress, masked, Presence fixture (exact) | 0.000 | 0.01 | 0.00 | +0.000 | 66.5 MB |
| Sharpen stress, masked, Z6 / X100VI / Air 2S | 0.022 / 0.000 / 0.021 | 0.09 / 0.00 / 0.12 | 0.71 / 0.00 / 0.69 | +0.000 / +0.000 / +0.000 | 107.6 / 103.2 / 134.6 MB |

Every cell is within the spatial limits: at worst a mean of 0.027, a worst block of 0.14 and a p99 of 0.76, on the Z6 and Air 2S, where the geometry tail's coordinate grid places the lens warp's samples, and no signed ΔL\* past ±0.001; on the X100VI the half-precision planes leave a mean of 0.011 and a worst block of 0.08, and on the JPEGs the GPU frame is within a worst block of 0.03 of the proxy. The Presence fixture fits the bounds at its own size, so its Fit frame is the exact render itself, with no proxy and no settle jump, and its GPU frame runs the full-resolution B3 levels. [The qualification](#the-corpus-error-report) measures the zone plate.

**Memory.** "Charged" is what the photo surface's slot drawing the plan charges the GPU-preview budget, as for Presence. Both units hold 48 bytes a pixel of half-precision planes, noise reduction 40 alone and sharpening 28 alone, each unit's apply reading a plane of its own that one pass writes ([design](../design/gpu-preview.md#plane-sharing-and-precision)). With the moderate settings on the 60 MP JPEG at Fit (a 1716 × 1030 proxy), the slot holds 115.8 MB (110.4 MiB) of the 2 GiB budget, of which the planes are 84.8 MB; over the evidence window's whole 1716 × 1508 bounds it would hold 161.7 MB (154.2 MiB). The RAWs charge more, their boundary held as `f32` and the Z6's and Air 2S's lens grid beside it: 173.8 MB at most, the Air 2S with moderate settings. One slot is held at a time, so this is its peak. A JPEG stage of 7000 × 4667 charges 2,035.0 MB (1,940.7 MiB) and fits; one past about 33 MP passes the budget (7200 × 4800 would charge 2,070.7 MiB) and takes the CPU path, naming it (`gpu_detail_planes_are_charged_to_the_budget`). At 100% a Detail slot charges 337.1 to 500.8 MB, and Detail then all three Presence fields 1,099.1 to 1,159.2 MB ([GPU previews at 100%](#gpu-previews-at-100), [after Detail, Presence](#after-detail-presence)).

### After Detail, Presence

A plan chains every spatial and restoration layer after its boundary in recipe order ([GPU previews](../design/gpu-preview.md#where-the-code-lives)), so a Detail drag with Presence in the stack, or a drag of a layer before both, is drawn on the GPU. The corpus's three chained recipes, in the Detail family, ran through `gpu_detail_corpus_at_fit` (`LUXFORGE_GPU_CORPUS_RECIPES=detail-presence,detail-presence-local,detail-presence-masked`): Detail at the moderate settings, then Presence with all three fields at +50 / +50 / +30, with Texture and Clarity alone, and with all three through the radial mask. The plan starts at the Detail layer and holds Detail's operation, then Presence's. Each cell was judged as the Detail cells were, against the CPU's moving proxy. All 18 measured cells meet the spatial limits; [the qualification](#the-corpus-error-report) measures the zone plate.

| Recipe, source | Mean ΔE00 | Worst block | p99 | Signed ΔL\* | Charged |
| --- | ---: | ---: | ---: | ---: | ---: |
| All three fields, 24 MP / 60 MP JPEG | 0.001 / 0.054 | 0.04 / 0.26 | 0.00 / 0.26 | +0.000 / −0.019 | 206.4 / 187.5 MB |
| All three fields, Presence fixture (exact) | 0.004 | 0.06 | 0.25 | −0.000 | 150.3 MB |
| All three fields, Z6 / X100VI / Air 2S | 0.032 / 0.020 / 0.039 | 0.14 / 0.13 / 0.15 | 0.96 / 0.61 / 0.87 | +0.000 / +0.000 / +0.000 | 211.3 / 237.8 / 269.1 MB |
| Texture and Clarity, 24 MP / 60 MP JPEG | 0.001 / 0.001 | 0.06 / 0.05 | 0.00 / 0.00 | +0.000 / +0.000 | 200.3 / 182.1 MB |
| Texture and Clarity, Presence fixture (exact) | 0.003 | 0.05 | 0.20 | −0.000 | 146.0 MB |
| Texture and Clarity, Z6 / X100VI / Air 2S | 0.029 / 0.018 / 0.036 | 0.13 / 0.11 / 0.14 | 0.96 / 0.59 / 0.85 | −0.000 / +0.000 / −0.000 | 206.7 / 231.7 / 263.1 MB |
| All three fields, masked, 24 MP / 60 MP JPEG | 0.001 / 0.001 | 0.08 / 0.26 | 0.00 / 0.00 | +0.000 / −0.000 | 206.4 / 187.5 MB |
| All three fields, masked, Presence fixture (exact) | 0.004 | 0.04 | 0.25 | −0.002 | 150.3 MB |
| All three fields, masked, Z6 / X100VI / Air 2S | 0.020 / 0.012 / 0.024 | 0.13 / 0.08 / 0.13 | 0.70 / 0.53 / 0.73 | −0.000 / +0.000 / −0.000 | 211.3 / 237.8 / 269.1 MB |

The figures are Detail's and Presence's own, Detail's half-precision planes included ([plane precision](#plane-precision)): at worst a mean of 0.054 and a signed ΔL\* of −0.019 on the 60 MP JPEG, where Dehaze's light is the stage's, a worst block of 0.26 there too, and a p99 of 0.96 on the Z6, where the lens warp's grid places the samples (all within the spatial limits of 1.0, 2.5, 5.0 and ±0.5). Each operation is a link of its own with its own planes, so the slots charge 146.0 to 269.1 MB at Fit.

**At 100%.** In the largest window the owner's display holds, a 3026 × 1826 region ([GPU previews at 100%](#gpu-previews-at-100)), Detail then Texture and Clarity draws on the GPU on every source, and so does Detail then all three fields. Dehaze's estimate sits behind Detail; these figures were measured with the light the whole exact frame stored, and its cells against the exact whole frame's region. A region plan reads the light its light link computes, with Detail left out until a slot sweeps the exact prefix, which the [release gate](#gpu-qualification-against-the-reference) measures. Each slot is in the GPU's shape, every unit held, and the window is the region grown by the halos after the boundary alone, with no tile grid ([design](../design/gpu-preview.md#at-100-and-above)); Detail's link holds its half-precision planes and Presence's link its own, Texture's at full precision, with the intermediate Detail's link writes between them ([plane precision](#plane-precision)). Charged, against the 2,147.5 MB (2 GiB) budget, and the headroom it leaves:

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
- **A larger window**, such as an external display's, draws a larger region. The charge grows with the window's area, and from the measured charges the all-three chain stops fitting at a region about 36% wider and taller on the Air 2S (about 4120 × 2485), 38% on the X100VI, 39% on the Z6 and 40% on the 60 MP JPEG, and Texture and Clarity about 44% on the Air 2S (an estimate from the charges above, not a measurement). Such a region's drag is drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging", `budget-reduced`), and is sharp at rest ([GPU previews](../design/gpu-preview.md#at-100-and-above)). The slot's scratch pool leaves this chain as it is, since Detail's planes with noise reduction share no texture class with Presence's; the [design](../design/gpu-preview.md#later) records the proposal that would widen the headroom, evaluating the ring only Clarity's and Dehaze's reductions read in strips. With Texture's band in one channel, the corpus's heaviest slot, all three fields on the Air 2S, charges 1,123.8 MB ([the corpus](#the-corpus)).
- **A slot that replaces another** is charged only once the one it replaces has retired, when the GPU is done with it: a tick whose slot the budget holds only without the old one draws the CPU's frame, naming `budget-exceeded`, keeps its boundary and plan and asks for nothing again, and the next frame after the retirement draws on the GPU; nothing is allocated or released twice (`a_larger_plan_waits_for_the_planes_it_replaces_then_holds_its_own`, `gpu_preview_a_tick_the_budget_refuses_while_a_slot_retires_keeps_its_boundary`).

**Ticks.** Over Detail then Presence (Texture and Clarity), 27 passes, a tick runs only the passes its words change: a Clarity drag none, a Texture drag 5, a drag of the colour layer between the two Presence's 13, and a Detail drag 17, each frame equal to the bit to a run of every pass, each operation a link of its own (`gpu_presence_after_detail_reruns_only_the_passes_a_tick_changes`). Against the CPU frame of the same stacks on a synthetic photograph at two sizes, Detail's sharpening and noise reduction then three Presence settings, plain and masked, with Dehaze's light stored and taken on the GPU, the worst of 40 cases is a mean of 0.068, a worst block of 0.38 and a p99 of 0.80, its signed ΔL\* −0.001 (`gpu_presence_after_detail_meets_the_spatial_limits`): Detail's half-precision planes, and Detail's output held in the link's `rgba16float` intermediate between the two, each written rounded to the nearest half.

### A drag at Fit

A Detail drag at Fit drawn on the GPU is proven functionally, not timed:

- `cargo test -p luxforge-app gpu_detail_tests::drags` against a real owner and preview worker. In the first test, a Detail Amount drag over a photograph drawn at Fit derives its boundary, the Detail layer's input, the source reduced to the view, with its first tick and holds the frame on screen until the surface has evaluated its plan. Its later ticks are Detail's spatial step drawn on the GPU with no preview job. The release commits: the committed stack renders no proxy, its exact frame reduced to the view is the reference frame, and the GPU draws its picture at rest; the boundary stays resident for the next draft. In the second, a Basic drag after a committed Detail layer starts from that resident boundary, the stack's first layer's input, asking for none, and its ticks run Detail's spatial step, which the surface keeps by content, then Basic's colour, drawn on the GPU with no preview job.
- `cargo run --release --locked --package xtask -- smoke --scenario gpu-preview --output NEW_DIR` with the Detail steps: every tick drawn on the GPU, the first included, over the resident boundary at layer 0 that the Basic drags before it left, with no boundary request and no preview job. The GPU frame equals the frame the release commits at four flat patches and two across the white cross, to the code. The slot held 16.4 MB over the 480 × 320 photograph, which fits the window at its own size, so its Fit frame is the exact render, and holds it once settled, the boundary resident for the next gesture.

### Reproducing it

```sh
cargo test -p luxforge-app gpu_detail -- --nocapture
```

The corpus harness, `gpu_detail_corpus_at_fit`, compared against the CPU proxy and is not in the repository. The release gate runs the Detail family, its chained recipes among them, at Fit against the reference renderer's frame, not the CPU proxy or the exact-derived frame, so it reproduces the comparison under the current contract, not these figures, as [for the colour programs](#gpu-colour-programs-at-fit):

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom fit --kind picture-at-rest,picture-in-motion \
  --families detail
```

## GPU previews at 100%

The GPU frame of a drag at 100% ([GPU previews](../design/gpu-preview.md#at-100-and-above)) against the CPU's exact visible region, a frame the editor does not draw, on every family of the [corpus](../../fixtures/preview/corpus.json) over every source this host has, in the largest window the owner's display holds. Pixel measurements and charges, not timings. The GPU draws the picture at rest over the visible region too, which the [release gate](#gpu-qualification-against-the-reference) holds to the reference renderer's frame.

### Scope

- **The view.** The M4 MacBook Pro's 3024 × 1964 display, a 1512 × 982 logical window at 2× with both panels closed, at 100% and scrolled to the photograph's centre: a 3026 × 1826 region of the output stage (`viewport_rect`, with its guard pixels), or the whole stage where it is smaller (the Presence fixture's 1440 × 960). This is the largest window the owner's display holds, and so the largest region the corpus measures; a larger window, such as an external display's, draws a larger region, whose slot may pass the budget ([after Detail, Presence](#after-detail-presence)).
- **The frames.** The CPU frame was the preview worker's exact region of that view, or, for a stack whose region the worker declined and rendered whole (an estimate behind an earlier spatial layer), that region of the exact whole frame the view drew it from. The GPU frame was the plan from the recipe's first pixel layer at the exact stage over the boundary the worker renders for a region job (`qualification::region_boundary`, `Render::region_boundary`: the window of the layer's received stage the region reads on the GPU, each spatial operation after the layer by its halo alone, with the exact region's whole-stage estimates), converted with the region (`surface_plan_over`), a lens warp's grid over the region at 100%, and drawn by the photo surface's own shader, read back headlessly. Both are compared at the region's size. A Detail or Presence layer is planned as a drag of it draws: in its GPU shape, every unit, while that slot fits the budget, and in the CPU's shape, the units its values need, when only that one does.
- **What a Detail frame is judged against.** The exact visible region. At Fit a Detail drag is judged against the CPU's moving proxy, the frame the GPU stood in for; at 100% the CPU's own frame is the exact region, so there is nothing between them.
- **Gaps.** A slot the surface would charge past the GPU-preview budget, or a boundary past the 256 MiB bound on one, is a gap named `budget-exceeded`. A stack with Dehaze behind Detail is measured against the exact whole frame's region, the frame the view draws. These figures were measured with Dehaze's light read from the estimate store the exact render filled, the exact stage's; a region plan reads the light its light link computes from the whole stage, the same light within 2.9 × 10⁻⁶ for a prefix of colour alone ([design](../design/gpu-preview.md#spatial-programs)). A mask that selects nothing inside the region is a gap, as at Fit.
- **Host and build.** Apple M4 Pro, macOS 26.5.2, the `Apple M4 Pro` adapter on Metal; the `test` profile build of the chain of links with a region's frame held in its own picture's bucket, `gpu_preview_corpus_at_100_percent`, under the 2 GiB budget and the 256 MiB bound on a boundary, reading the store, with Detail's half-precision planes ([plane precision](#plane-precision)), each spatial operation a link of its own. One run; the pixels are deterministic.
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
- **The 60 MP JPEG in the largest window.** Its slots charge 67.0 MB for a colour or masked stack, 82.6 MB through a perspective warp, 107.6 MB through the straightened crop, and for Presence, every unit held whichever fields are moved, 252.5 MB for Texture, 290.9 MB for Dehaze, 393.0 MB for Clarity, 400.1 MB for Texture and Clarity and 456.0 MB for all three fields, masked or not: each window is the region grown by the halos alone, with no tile grid. Detail's slots, every unit, charge 337.1 to 351.5 MB; Detail then Texture and Clarity 933.5 MB and Detail then all three fields 1,099.1 MB, each operation a link of its own with its own planes and the intermediate Detail's link writes between them ([after Detail, Presence](#after-detail-presence)). Each holds its frame in the bucket of the region's own picture, 3072 × 1856 texels, where the photograph's square bucket of 3072 × 3072 would charge every one of them 14.9 MB more.
- **The RAWs.** A colour, masked or warp slot charges 111.2 to 251.6 MB, a rotated crop's up to 401.4 MB, a geometry tail's `rgba32float` intermediate among them, and Presence's, every unit held, 295.9 to 391.4 MB for Texture, 333.6 to 429.7 MB for Dehaze, 423.9 to 510.4 MB for Clarity, 429.8 to 516.3 MB for Texture and Clarity and 481.3 to 566.4 MB for all three fields, masked or not, all measured in the GPU's shape. Detail's slots charge 382.1 to 500.8 MB, every unit, Detail then Texture and Clarity 986.1 to 1,032.3 MB and Detail then all three fields 1,114.3 to 1,159.2 MB, the peak of the measured cells.
- **Detail.** Every measured cell is within a fraction of the spatial limits, its half-precision planes included: a worst block of 0.26, a p99 of 0.90 and a mean of 0.052, each on the Air 2S, Detail then all three fields, against 2.5, 5.0 and 1.0, and on the JPEGs and the zone plate a worst block of 0.12 at most.
- **Dehaze.** With its light taken on the GPU from the region alone, the Z6's frame missed the spatial limits (worst block 13.14, p99 2.87, mean 0.76) and the X100VI's (worst block 3.20): the region's brightest blocks are not the whole stage's, and the exact region reads the whole stage's light. With the whole stage's light every Dehaze cell meets the spatial limits (worst block 0.29 and p99 0.81 on the Air 2S), which a region plan's light link computes ([lights taken over less than the whole stage](#lights-taken-over-less-than-the-whole-stage)).
- **Presence drags at 100%.** In the `gpu-preview-zoom` scenario, over a committed Dehaze and Clarity at 100%, Presence runs 20 passes, its light link computing the light beside them, the first draw over a new boundary running every pass (`gpu_preview_spatial_passes`). The scenario draws a Basic drag under that Presence on the GPU, its light computed every tick, and, with Detail committed under Presence and Dehaze, a Texture and a Detail drag reading the light the picture at rest computed from Detail's output; those steps are unrun ([GPU previews](../design/gpu-preview.md#at-100-and-above)).

### Reproducing it

The harness that measured these figures, `gpu_preview_corpus_at_100_percent`, compared against the CPU region and is not in the repository. The release gate runs every family at 100% over the visible region of the owner's largest window against the reference renderer's frame, not the CPU region, so it reproduces the comparison under the current contract, not these figures; `--recipes` measures only the recipes it names. A selection, so the run reports itself incomplete:

```sh
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --zoom 100 --kind picture-at-rest,picture-in-motion
```

### Lights taken over less than the whole stage

A lesson the per-frame light rests on ([design](../design/gpu-preview.md#spatial-programs)). Five lights other than the whole exact stage's were measured against the exact frame a drag settles to, over the sources the corpus measures Detail beside Presence on at 100% and behind the corpus's straightened crop at Fit (Apple M4 Pro, Metal; the harness is not in the repository). Each missed the spatial limits where its stage was not the whole exact one:

- **A proxy's own light**, the drafted stack's Fit proxy or the CPU's moving frame: their 16 px blocks cover more of the picture than the exact stage's, missing a Presence drag by up to a mean of 8.78 (the Fit proxy's) and 5.34 (the moving frame's).
- **A window's light**: over a windowed Fit proxy behind a straightened crop, the light of the window or of the whole proxy stage missed by a mean of 2.1 to 17.9 on the 60 MP JPEG; over a 100% region, the region's own light missed on the Z6 and X100VI.
- **The light of the stack a drag started from**, held for the drag: within the limits for a Detail drag while Dehaze removes a veil (35 of 35 cells), but at Dehaze −100 sharpen stress missed on the zone plate (signed ΔL\* −1.15) and the Air 2S (+2.34), and under a Basic drag it missed on 25 of 49 cells, at three stops of exposure worst (a mean of 60.9).
- **A light from a held reduced stage**, the drafted colour run over Dehaze's own 16 px reduction of the dragged layer's input: 44 of 49 Basic cells, missing at three stops under every field at ±100 (a mean of up to 1.90, a signed ΔL\* of −1.67), and behind the crop at ±3 EV on the 60 MP JPEG. The [reduction factor](#the-per-frame-lights-reduction-factor) measured the same gap at every coarser factor.

The exact stage's light was within a mean of 0.065 and a worst block of 0.54 on every cell. So every light is computed from the whole content stage at full resolution.

### A boundary's arrival

What a boundary's arrival holds at once — the desktop's copy, wgpu's upload staging and the texture — with the upload spread at `UPLOAD_PER_FRAME`. A functional measurement, not a timing: `a_boundary_arrival_measured` hands the photo surface the largest `f32` boundary a RAW region with Clarity's margin reads in the largest window, 4705 × 2817 texels (212.1 MB), frame by frame until the surface holds it, then lets its copy go as the desktop does, recording each frame's staged bytes, the slot's charge and the process's footprint, and the process's own peak footprint, which catches what falls between frames. Apple M4 Pro on Metal, the `test` profile build, in its own process, one run.

| | Frames | Staged a frame | Footprint during the upload | At the frame that draws it | Process peak | After the copy is let go |
| --- | ---: | --- | --- | --- | --- | --- |
| Spread, 32 MiB a frame | 7 | 33.6 MB, the last 10.6 MB | +463.1 to +471.8 MB | +576.1 MB | +576.3 MB | +565.5 MB |

The footprint is above the process's before the boundary, which holding the copy alone raises by 212.2 MB. The slot charges 316.9 MB: the boundary's texture and the output's, 104.8 MB in its size bucket, which the drawing frame writes, a full-size output this measurement draws where a region plan's is the region's. Without that output, the arrival held 2.2 times the boundary (the copy, the texture and a frame's staging, at most 471.8 MB). After the arrival the footprint stays near its peak: the system allocator keeps the copy's freed pages for its next block of that size (the measurement holds the copy as the only reference when it lets it go).

```sh
cargo test -p luxforge-ui --lib a_boundary_arrival_measured -- --ignored --nocapture
```

## Painting over masked spatial layers

Brush strokes over masks that hold Clarity and Texture, drawn through the GPU preview's chain of links, its scratch pool, its masked passes and its incremental ticks ([design](../design/gpu-preview.md#where-the-code-lives)): every link's scratch planes in the slot's one pool under one charge, Texture's band in one channel, and at most 16 masked spatial layers with their compile cache and warm list. Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2 (25F84), the `Apple M4 Pro` adapter on Metal, release builds, on a host shared with other sessions, with background applications running: the load column is the one-minute load at each run's start, and a run above the 8.0 threshold is marked. Megabytes are 10⁶ bytes.

### A stroke's latency

`editor-latency --mode paint --samples 120 --window 1728x1080`: a full-screen window on the 3456 × 2160 display at 2×, whose Fit stage of the generated 24 MP JPEG is 2292 × 1528. 120 positions of the curved stroke, one every 24 ms, on the brushed mask (size 0.06, feather 50), which holds a masked exposure of +0.6 EV and, with `--mask-presence`, a masked Presence layer of Clarity 50 and Texture 40; `--masks N` adds N − 1 radial masks across the frame holding the same, and the recipe holds every masked Basic layer first, then every masked Presence layer. `--zoom 100` paints at 100%, and the X100VI runs, through the private RAW manifest, add a global Detail layer (`--detail`). Milliseconds, nearest rank, from each position's input to the frame that shows it (`position_to_presented_frame`); *early* and *late* are the stroke's first and last 30 positions; the GPU-preview peak is `gpu_preview_peak_bytes` over the run and the scratch pool `last_gpu_preview_scratch_bytes`. Each case ran twice.

| Run | Load | Frames | Position to frame p50 / p95 / max | Early p95 | Late p95 | Press to first frame | GPU-preview peak | Scratch pool |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Fit, 3 masks, 1 | 3.03 | 120 GPU | 7.8 / 8.5 / 8.9 | 8.5 | 8.7 | 1.5 | 257 MB | 74 MB |
| Fit, 3 masks, 2 | 4.39 | 120 GPU | 7.9 / 8.9 / 9.3 | 8.8 | 9.0 | 1.3 | 257 MB | 74 MB |
| 100%, 3 masks, 1 | 5.01 | 118 GPU, 2 CPU (`boundary-pending`, `boundary-uploading`) | 7.8 / 8.9 / 73.6 | 49.9 | 8.2 | 73.6 | 542 MB | 168 MB |
| 100%, 3 masks, 2 | 4.88 | 119 GPU, 1 CPU (`boundary-pending`) | 7.6 / 9.3 / 60.9 | 24.6 | 9.0 | 60.9 | 542 MB | 168 MB |
| X100VI at Fit, Detail and 3 masks, 1 | 6.40 | 120 GPU | 7.9 / 8.9 / 10.4 | 8.9 | 9.0 | 10.4 | 557 MB | 186 MB |
| X100VI at Fit, Detail and 3 masks, 2 | 6.52 | 120 GPU | 7.7 / 8.8 / 10.8 | 8.5 | 8.8 | 1.7 | 557 MB | 186 MB |
| Fit, 10 masks, 1 | 7.61 | 120 GPU | 8.3 / 23.4 / 25.2 | 23.9 | 9.3 | 11.8 | 558 MB | 74 MB |
| Fit, 10 masks, 2 | 8.44 (above 8.0) | 120 GPU | 8.3 / 22.8 / 23.7 | 22.8 | 10.9 | 2.6 | 558 MB | 74 MB |
| Fit, 16 masks, 1 | 10.61 (above 8.0) | 120 GPU | 9.2 / 24.4 / 36.5 | 24.0 | 24.2 | 4.0 | 815 MB | 74 MB |
| Fit, 16 masks, 2 | 13.99 (above 8.0) | 120 GPU | 8.8 / 24.3 / 34.4 | 9.6 | 11.4 | 4.2 | 815 MB | 74 MB |

- **Three masks, within a frame.** p95 is 8.5 to 8.9 ms at Fit, 8.9 to 9.3 at 100%, and 8.8 to 8.9 on the X100VI with Detail. Every position at Fit is drawn on the GPU at the redraw after its input, the press 1.3 to 10.4 ms from its input to its frame where the stack's resident boundary is held ([the GPU source](../design/gpu-preview.md#the-gpu-source)).
- **The GPU-preview peak** with three masks is 257 MB at Fit, 542 MB at 100% and 557 MB on the X100VI with Detail, where Detail's half-precision planes share no texture class with Presence's, so the pool holds both links' scratch, 186 MB.
- **10 and 16 masks at Fit**: every position is drawn on the GPU, but p95 is 22.8 to 24.4 ms, **missing the 16 ms target**, because a tick's GPU work passes the 120 Hz frame ([what a tick costs](#what-a-tick-costs-the-gpu)). The loads above 8.0 on most of these runs came largely from the runs' own settled CPU renders of 10 to 16 masked layers ([the settled render](#the-masked-spatial-primitive-one-to-sixteen-layers)). The pool stays 74 MB, one link's scratch, at every count.
- **10 and 16 masks at 100%**: the window grows with every chained link ([the window](#the-100-window-grows-with-the-chain)), so the region's slot passes the budget, and a drag there is drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging", `budget-reduced`), sharp at rest.
- **The first stroke after a zoom.** At 100% with three masks the first one or two ticks are the CPU's while the region's boundary renders and uploads: the press reaches the screen in 61 to 74 ms and the early positions' p95 is 25 to 50 ms. Once the boundary is held every tick is the GPU's, late p95 8.2 to 9.0 ms. The harness zooms over a retained exact frame just before it paints, so no view job asks for the region's resident boundary first ([later](../design/gpu-preview.md#later)).

### What a tick costs the GPU

`gpu_mask_a_painted_stroke_costs_where_it_changes` (release, `--ignored --nocapture`, the qualifier's headless device on the same adapter): the same stroke's 119 ticks over a brushed mask and two radials, each holding a masked Basic and a masked Presence layer, every tick after the first submitted without waiting, so the figure is the GPU's throughput a tick. The host's decimation moves some of the stroke's kept positions at each tick, so a tick's change covers 6.3% of the Fit stage and 7.1% of the region on average. Two runs, at loads of 2.2 to 2.5:

| Run | Fit, 2292 × 1528: incremental / whole, a tick | 100% region, 2994 × 2642: incremental / whole, a tick |
| --- | ---: | ---: |
| 1 | 2.37 / 6.55 ms | 4.31 / 14.66 ms |
| 2 | 2.34 / 6.57 ms | 4.33 / 14.61 ms |

Encoding takes 0.06 to 0.10 ms a tick in every run. The incremental ticks take 2.8 to 3.4 times less GPU time than whole ones and draw what whole ones do, bit for bit (`gpu_mask_a_painted_stroke_is_evaluated_where_each_tick_changes_it`).

**The paint harness's layout at Fit** (`gpu_mask_the_paint_harness_layout_costs_a_tick_at_fit`, release, load about 4.6 to 4.9): the 2292 × 1528 Fit stage of a 6000 × 4000 photograph, the masked Basic layers then the masked Presence layers, the stroke on the first mask, so every Presence link after it is evaluated incrementally. The slot charges 257.4, 557.7 and 815.2 MB, the paint runs' peaks to the MB.

| Masks | Incremental, a tick (encoding) | Whole, a tick (encoding) | One tick alone p50 / p95 / largest |
| ---: | ---: | ---: | ---: |
| 3 | 2.04 ms (0.10) | 6.88 ms (0.06) | 2.77 / 5.30 / 6.64 ms |
| 10 | 8.80 ms (0.21) | 17.25 ms (0.13) | 10.80 / 14.49 / 15.75 ms |
| 16 | 14.46 ms (0.25) | 26.40 ms (0.20) | 17.32 / 21.06 / 23.76 ms |

At 10 and 16 masks a tick's GPU work passes the 8.33 ms frame, which is the paint runs' 23 to 24 ms p95. The pool does not add to it (above): it is the work of 10 to 16 chained Presence links, each evaluated incrementally around a change that grows link by link with each link's reach ([later](../design/gpu-preview.md#later)).

### The scratch pool's memory

What a masked Presence layer costs the GPU-preview budget with every link's scratch planes in the slot's one pool and Texture's band in one channel ([design](../design/gpu-preview.md#plane-sharing-and-precision)). Deterministic, one run: `cargo test --release -p luxforge-app --bin luxforge gpu_shared_scratch_measured -- --ignored --nocapture`, the slot's charge (`Qualifier::charged_bytes`, which the live slot's `gpu_preview_in_use_bytes` equals) for 1, 2, 4, 8 and 16 masked Presence layers, each through a radial of its own. At Fit, a 2292 × 1528 boundary against a 6000 × 4000 stage; at 100%, a fixed 3778 × 2578 window at (1111, 711) of the 6000 × 4000 exact stage, the region drawn being the window itself. A JPEG's boundary is `rgba16float` (the byte path); a RAW's is `rgba32float` (the linear path), as are its intermediates. Megabytes, against the 2 GiB budget of 2,147.5 MB:

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

- **The pool** is charged once, whatever the number of layers: one link's scratch, 74.4 MB at Fit and 207.0 MB over the window for Texture and Clarity.
- **The one-channel band** is 4 bytes a pixel of every Texture layer: 14.0 MB at Fit and 39.0 MB over the window, in the first layer's charge and in every increment.
- **The fixed window understates a real chain at 100%**, since the window grows with every chained spatial layer, below.

#### The 100% window grows with the chain

`gpu_window_the_paint_harness_masks_at_100_grow_the_window_by_every_links_halo` and `gpu_window_the_paint_harness_masks_at_100_fit_the_budget_up_to_a_count` (release, ignored), through the real planning path: the editor opens the generated 24 MP JPEG (6000 × 4000), another client commits the paint harness's layout, and a drag in Mask mode at 100% asks for its plan. At a percentage zoom the boundary is the view's region grown by the halo of every spatial layer chained after it (`WindowPlan::of_gpu_rect`), so each masked Presence link adds 207 px on every side (Texture 8 and Clarity 199), and every link's intermediate and kept planes and the pool are sized to that window. `plan_steps` converted every plan, and `region_charge` equals the surface's own `slot_charge` but for the links' buffers (6 to 40 KB). Budget 2,147,483,648 bytes. In the harness's view, 1728 × 1080 at 2× with the panels open, scrolled to the top left (the region 2373 × 2021 at (0, 0)):

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
- **With Dehaze on the masks** the drag changes every Dehaze layer's input, so each tick computes every layer's light from the whole stage, a light link a layer charged beside the slot; this measurement's Dehaze case is unmeasured.
- **No defect.** For one window shared by every link and blind to masks, this growth is what an exact region frame needs. It over-estimates in two ways, each a proposal that is not built, with its estimated effect in the [design](../design/gpu-preview.md#later): growth bounded by each link's mask, and each link's intermediate and kept planes sized to its own output's need.

### The corpus

The five corpus harnesses, release profile, with every source the corpus names, the zone plates and the three RAWs through the private manifest among them; the pixels are deterministic. Worst of each statistic over the measured cells — mean ΔE00, worst 16 × 16 block, p99 and \|signed mean ΔL\*\| — against the pointwise limits (0.5, 1.0, 2.0, 0.25) or the spatial ones (1.0, 2.5, 5.0, 0.5):

| Harness | Within the limits | Pointwise | Spatial | Gaps | Heaviest slot |
| --- | ---: | ---: | ---: | --- | --- |
| Colour at Fit | 62 of 62 | 0.051 / 0.25 / 0.87 / 0.002 | — | 2: a vignette after a RAW's lens warp | — |
| Masks at Fit | 42 of 42 | 0.021 / 0.12 / 0.66 / 0.009 | — | none | — |
| Presence at Fit | 81 of 81 | — | 0.061 / 0.51 / 0.97 / 0.049 | none | `crop-presence-all` on the Z6, 245.6 MB |
| Detail at Fit | 63 of 63 | — | 0.054 / 0.26 / 0.96 / 0.019 | none | Detail then Presence on the Air 2S, 261.2 MB |
| Every family at 100% | 247 of 247 | 0.051 / 0.25 / 0.85 / 0.009 | 0.084 / 0.51 / 0.95 / 0.049 | 3: masks that select nothing in the region (`mask-radial` and `mask-brush` on the 60 MP JPEG, `mask-brush` on the Z6) | Detail then all three Presence fields on the Air 2S, 1,123.8 MB |

Under the 2 GiB budget every 100% slot draws on the GPU. The pool does not share Detail's scratch with Presence's, since their planes share no texture class; the one-channel band lowers the heaviest slot.

### The performance-rules checklist

For the chain, the masked passes, the incremental ticks and the resident boundary ([rules](../engineering/performance-rules.md#review-checklist)):

- **The original** is read, hashed and decoded only through the verified prepared source.
- **Full-frame allocations.** On the GPU, each link's intermediate (the boundary's size and format), each masked spatial layer's planes and a plane of its own for each unit's result, and the resident boundary held between gestures, all within the 2 GiB GPU-preview budget, which refuses a slot past it and names the CPU path. Nothing is cloned that is shared.
- **Point queries, validation and no-op checks** render no frame.
- **The owner thread** plans a committed stack's own plan and its warm list at its view, a settled 100% view's among them, from the compiled stack's descriptions in `O(layers × modules)`: no frame work. The interface thread compares each tick's plan with the plans it handed (`GpuPlan::changes_since`) in `O(units + components + segments)`, reading no pixel.
- **Desktop messages.** A tick drawn on the GPU sends no preview job and uploads no frame. A committed job asks for the resident boundary only when no held boundary has its key, and a view job asks for the region's plans only when it settles.
- **Timers, polls and subscriptions.** None added.
- **Repeated work.** A link whose input and words did not change is not run (keyed by content: its input's key, words, blocks and pipeline); a tick runs each changed link only where its change reaches; a masked spatial layer runs its passes only over its mask; the resident boundary is rendered once per stack and view rather than once per gesture.
- **`editor-performance`.** Not run: the chain changes no CPU render.
- **Exactness.** Partial and incremental evaluation are held to whole evaluation bit for bit (`gpu_presence_a_masked_layer_runs_its_passes_over_its_mask_alone`, `gpu_mask_a_painted_stroke_is_evaluated_where_each_tick_changes_it`, `gpu_mask_a_moved_radial_is_evaluated_where_its_bounds_were_and_are`, `gpu_mask_a_mask_grown_toward_the_edge_reads_its_inputs_latest_values`); a plan over another boundary or with other clipping marks is evaluated whole (`a_change_measured_over_another_boundary_is_evaluated_whole`, `a_change_is_measured_only_over_the_same_boundary_and_marks`).

### The performance-rules checklist for the scratch pool and sixteen layers

For the slot's scratch pool, the one charge, Texture's band in one channel, the fallback notice, and the cap of 16 masked spatial layers with its compile cache and warm list ([rules](../engineering/performance-rules.md#review-checklist)):

- **The original** is not read: none of these reads a source.
- **Full-frame allocations.** On the GPU, one pool of scratch textures a slot, each the boundary's size, its blocks or a fixed size, which every link takes in turn rather than holding a set of its own; a link's kept planes and intermediate; and Texture's band in a one-channel `r32float` plane, 4 bytes a pixel. The pool is charged once to the 2 GiB GPU-preview budget, refused before anything is created past it, and retired through the surface's retirement worker, charged until the GPU is done with it. At three masks the GPU-preview peak is 257 to 557 MB, and 16 masked Presence layers charge 815 MB at Fit ([memory](#the-scratch-pools-memory)). Nothing is added on the CPU: the notice holds one phrase and tooltip, and the warm list plan descriptions.
- **Point queries, validation and no-op checks** render no frame. The cap is checked when the stack compiles, `region_charge` is arithmetic over the plan's steps converted with no boundary and no device, and the notice reads the reason the evidence already records.
- **The owner thread** plans the warm list, one drag for each distinct drafted shape within 45 link sequences, from the compiled stack's descriptions: no frame work. The interface thread fits the pool once a tick from every link's steps, in `O(links × planes)` as fitting the planes is, and encodes a tick in 0.06 to 0.25 ms on the qualifier's device over 3 to 16 masks ([what a tick costs](#what-a-tick-costs-the-gpu)); the editor's own preparation figure (`gpu_preview_frame_us`) is not recorded for these runs.
- **Desktop messages.** None added: a tick drawn on the GPU sends no preview job and uploads no frame, a slot the desktop's estimate refuses asks for no boundary, and the notice is derived in the update each tick already runs, a change of drawing path waking the desktop once. `asset.state` and `history.list` are unchanged.
- **Timers, polls and subscriptions.** None added. The half second before `compiling` is said is measured at ticks, so a drag that holds still keeps what it last said and the idle editor stays asleep.
- **Repeated work.** A pool texture's key is trusted only by the schedule that wrote it, so a link runs a pass again only where another link wrote its scratch since it last ran: a Dehaze drag runs 20 of 22 passes after another masked Presence link has run, and 19 alone. A painted tick's GPU cost is 2.34 to 2.37 ms at Fit and 4.31 to 4.33 over a 100% region. The compile cache keys a link's program sequence by its steps and the format it writes, bounded at 64 sequences (`PIPELINE_CACHE`), which hold the largest plan's 19 links beside a whole warm list of at most 45. Its hit rate is held by tests rather than measured: a plan of 16 masked layers of mixed shapes compiles each sequence once, then draws every tick on the GPU, and a drag of each of the 16 finds every link it draws warmed. A rebuild is a Presence link's compile, about a second on a cold shader cache ([compile cost](#gpu-preview-compile-cost)); the cache's retained bytes are not measured.
- **`editor-performance`.** Not run: these change no CPU render. The settled render the cap allows is measured: 16 whole-frame masked layers take 2.4 s at 24 MP and 6.7 s at 60 MP ([the masked spatial primitive](#the-masked-spatial-primitive-one-to-sixteen-layers)), and a render expected to take more than a second shows its progress.
- **Exactness.** Every frame is held bit for bit to a fresh evaluation through the slot and through the qualifier, over chained links whose ticks alternate and over 16 masked layers of mixed shapes, with the pool poisoned as well (`chained_links_take_their_scratch_from_one_pool_and_draw_what_a_fresh_slot_draws`, `gpu_presence_chained_masked_layers_dragged_in_turn_draw_what_a_whole_evaluation_does`, `gpu_presence_sixteen_masked_layers_of_mixed_shapes_draw_what_a_whole_evaluation_does`), and so is every test of partial and incremental evaluation; the one-channel band draws what a two-channel one does, bit for bit (`gpu_presence_texture_band_in_one_channel_draws_what_two_did`); the three charges agree (`gpu_window_a_chained_masked_plan_is_held_to_the_slots_own_charge`); and every corpus cell at Fit and at 100% is within its limits ([the corpus](#the-corpus)).

## GPU previews qualified on the M4

The native qualification of GPU previews ([design](../design/gpu-preview.md)): the corpus error report at Fit and at 100%, the latency of each gesture, memory, idle after a release, the Windows and Linux checks, the performance-rules checklist and the verify tiers. Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal. The corpus error report judges the GPU's frames against the CPU proxy and exact region they stood in for, which the editor does not render; the [release gate](#gpu-qualification-against-the-reference) holds the GPU to the reference renderer.

### The corpus error report

Every program and stack class of the [corpus](../../fixtures/preview/corpus.json) against the CPU frame it stood in for, over every source this host has: at Fit, the frame settlement presented ([colour programs](#gpu-colour-programs-at-fit) set out the frames); at 100%, the exact visible region, in the largest window the owner's display holds ([at 100%](#gpu-previews-at-100)). Five harnesses ran once each in the `test` profile; the pixels are deterministic, so the host's load does not bear on them. The sources are the generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture, and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest, all eight verified against the corpus's SHA-256 by `cargo xtask preview-corpus`. The family sections' own Z6 and Air 2S cells are drawn through a sparser lens grid than this report's. Every RAW cell through a lens warp, crop or perspective here was drawn with the tail's intermediate rounded to the nearest half, a signed ΔL\* of −0.004 to −0.010 darker, so those cells do not qualify the current `rgba32float` RAW geometry tail's accuracy or memory use ([plane precision](#plane-precision)).

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
- **Detail at Fit** is judged against the CPU's moving proxy it stood in for. Against the exact-derived frame at Fit, the GPU frame and the CPU proxy differ alike: both pass the spatial limits on 30 of 54 cells and miss them on the same 24, the zone plate's nine, the Air 2S's and those beside Presence among them (worst block 15.0 and p99 28.8 on the zone plate, 7.15 and 8.09 on a photograph).

### Gesture latency

Each case's 30-input drag through `editor-latency`: the release editor, the generated 24 MP and 60 MP JPEGs and the X100VI RAF, a hidden 1440 × 900 window at 2× on the 120 Hz display, warm filesystem cache. Each run started at a one-minute load between 3.5 and 6.9 (the column), all below the 8.0 threshold; the host carried only the owner's open applications. Each input is one step left open until its frame is on screen, so every figure is one input and one frame. Milliseconds, nearest rank; a drag's 30 inputs are 30 samples, the stroke's 400 positions 400.

- **What presented means on each path.** A CPU frame is presented at its hand-off, the update in which the worker's raster became the photo surface's source (`preview_displayed`); its draw follows at the next redraw. A GPU frame has no hand-off: it is presented at the surface's first draw of the plan tagged with the input's revision, the moment that draw is encoded (`surface_frame_drawn`). The GPU figure therefore includes the wait for the next frame, which the CPU figure does not; neither is scanout. **Drawn** times both paths to that first draw, the like-for-like column; a CPU region at 100% and above had no logged draw. On the 120 Hz display the figures fall at whole frames, 8.3 ms apart.
- **Frames.** Every drag's first input took the CPU path asking for its boundary (`boundary-pending`) and its later 29 were drawn on the GPU; its p99 is that first input.

| Case | Load | Frames | Presented p50 / p95 / p99 | GPU frames | CPU frames | Drawn p50 / p95 / p99 |
| --- | ---: | --- | ---: | ---: | ---: | ---: |
| 24 MP JPEG at Fit, full Basic | 6.86 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 9.3 / 17.5 | 8.3 / 8.6 / 9.3 | 17.5 / 17.5 / 17.5 | 8.3 / 9.3 / 26.3 |
| again | 4.64 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 9.5 / 17.8 | 8.3 / 8.4 / 9.5 | 17.8 / 17.8 / 17.8 | 8.3 / 9.5 / 27.6 |
| 60 MP JPEG at Fit, full Basic | 4.99 | 29 GPU, 1 CPU (boundary-pending) | 8.4 / 8.9 / 9.3 | 8.4 / 8.7 / 8.9 | 9.3 / 9.3 / 9.3 | 8.4 / 8.9 / 17.0 |
| again | 4.23 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 8.8 / 9.5 | 8.3 / 8.7 / 8.8 | 9.5 / 9.5 / 9.5 | 8.3 / 8.8 / 16.7 |
| 24 MP, five exports queued | 4.57 | 29 GPU, 1 CPU (boundary-pending) | 8.8 / 46.0 / 129.6 | 8.8 / 39.0 / 46.0 | 129.6 / 129.6 / 129.6 | 8.8 / 46.0 / 131.8 |
| 60 MP, five exports queued | 4.47 | 29 GPU, 1 CPU (boundary-pending) | 8.4 / 18.7 / 22.9 | 8.4 / 16.8 / 22.9 | 18.7 / 18.7 / 18.7 | 8.4 / 22.9 / 26.1 |
| 24 MP at 100%, full Basic | 4.97 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 9.4 / 9.6 | 8.3 / 9.3 / 9.4 | 9.6 / 9.6 / 9.6 | 8.3 / 9.3 / 9.4 |
| 24 MP at 200%, full Basic | 3.78 | 29 GPU, 1 CPU (boundary-pending) | 8.3 / 8.6 / 9.2 | 8.3 / 8.6 / 8.6 | 9.2 / 9.2 / 9.2 | 8.3 / 8.6 / 8.6 |
| X100VI, Texture drag over Presence | 4.77 | 29 GPU, 1 CPU (boundary-pending) | 8.5 / 9.5 / 43.5 | 8.5 / 9.3 / 9.5 | 43.5 / 43.5 / 43.5 | 8.5 / 9.5 / 46.4 |
| X100VI, Clarity drag over Presence | 3.64 | 29 GPU, 1 CPU (boundary-pending) | 8.4 / 8.6 / 42.7 | 8.4 / 8.5 / 8.6 | 42.7 / 42.7 / 42.7 | 8.4 / 8.6 / 44.2 |
| X100VI, Basic drag under Presence | 3.47 | 29 GPU, 1 CPU (boundary-pending) | 8.5 / 9.0 / 61.7 | 8.5 / 8.8 / 9.0 | 61.7 / 61.7 / 61.7 | 8.5 / 9.0 / 67.7 |
| 24 MP, a 400-position brush stroke | 4.34 | 399 GPU, 1 CPU (boundary-pending) | 7.1 / 8.5 / 9.2 | 7.1 / 8.5 / 9.2 | 10.5 / 10.5 / 10.5 | — |

- **Against the 16 ms p95 target.** Met at Fit at 24 MP and 60 MP (p95 8.8 to 9.5 ms over two runs each), at 100% (9.4) and 200% (8.6), by each X100VI Presence drag and the Basic drag under Presence (8.6 to 9.5), and by every position of the brush stroke (8.5). Each GPU frame is drawn at the first redraw after its input.
- **With an exact render holding the pool.** Five `export.write` jobs of the committed stack are queued just before the drag; the lane renders each exactly on the shared pool, then encodes and writes it, one at a time, and an input sent inside the lane's busy window is contended. At 60 MP all 30 inputs were contended: p50/p95/p99 8.4/18.7/22.9 ms, its GPU frames 8.4/16.8/22.9. **The 16 ms target is missed under contention**, by a frame: the interface thread that draws the GPU frame competes with the exports' renders for cores, so some frames wait one redraw more. The 32 ms acceptable limit is met. At 24 MP the five exports end within about a second, contending the first 8 inputs: p95 is 46.0 ms over all 30 and 39.0 over its 7 contended GPU frames, **missing both 16 and 32 ms**; its uncontended GPU frames also reached 46.0.
- **The brush stroke.** 400 positions, one every 24 ms, on one brush mask and its masked Basic layer: 399 drawn on the GPU, every position on screen with the first frame that carried it, 7.1/8.5/9.2 ms from its own input, the press 10.6 ms from the first position to its frame, none superseded.

### Memory

The GPU preview stage's high-water over each run (`gpu_preview_peak_bytes`), and the process's lifetime peak footprint as the Performance section last read it, beside the RSS `ps` sampled every 50 ms. The footprint peaks include the open, every exact render and histogram, the evidence captures and, in the contended run, the exports.

| Case | GPU-preview high-water | Process peak footprint | Sampled peak RSS |
| --- | ---: | ---: | ---: |
| 24 MP JPEG at Fit | 32.5 MB | 1569 MiB | 1176 MiB |
| 60 MP JPEG at Fit | 30.9 MB | 1727 MiB | 1573 MiB |
| 24 MP at 100% | 40.7 MB | 1811 MiB | 1287 MiB |
| 24 MP at 200% | 10.2 MB | 1727 MiB | 1255 MiB |
| 60 MP, five exports queued | 30.9 MB | 2148 MiB | 1697 MiB |
| X100VI, Texture over Presence | 95.9 MB | 2595 MiB | 2034 MiB |
| X100VI, Basic under Presence | 95.9 MB | 2658 MiB | 2080 MiB |
| 24 MP brush stroke | 32.5 MB | 1053 MiB | 665 MiB |

- **The GPU preview's own share** is 10 to 41 MB at Fit, 100% and 200% on the JPEGs and 96 MB over the X100VI's `f32` boundary with Presence, within the 2 GiB budget, which bounds the heaviest 100% slots the corpus measures (Detail then all three Presence fields, 1,123.8 MB with Texture's band in one channel; [the corpus](#the-corpus)) and the slots of a mask painted over masked Presence layers (0.26 to 0.82 GB at Fit over 3 to 16 masks and 0.54 GB at 100% over three, [painting](#painting-over-masked-spatial-layers)). A boundary arriving at 100% holds up to 2.2 times its bytes during its upload, and the system allocator keeps the freed copy's pages in the footprint afterwards ([a boundary's arrival](#a-boundarys-arrival)).
- **Against the provisional 1 GiB target for one 60 MP photograph**, which `measure` takes over one open: these gesture runs exceed it, 1.69 GiB peak footprint and 1.54 GiB sampled RSS for the 60 MP drag, of which the GPU preview's share is 31 MB. The peaks vary between runs by more than the GPU preview's share.

### Idle after a release

The 24 MP drag with `--idle`: with the Performance section closed, a 4-second settle and a 10-second window drew one frame and built one view, the window's own, and the process spent 15.1 ms of CPU, 0.15% of one core. The same catalog reopened in an ordinary launch, the Performance section open, used 0.83% of one core over 30 seconds, under the 1% idle target. `--idle` opens its window once the release's committed picture has settled, its counts adopted and, at Fit and below 100%, the GPU's picture at rest drawn ([development](../engineering/development.md#evidence-scripts)).

### Windows and Linux

This qualification ran only on the M4. Container software checks and hosted results are recorded in [development](../engineering/development.md#ci): Linux no-adapter checks, release acceptance and packaging pass; the retained lavapipe journeys use small GPU-stage fixtures and run to aggregate failure, with a passing hosted result still open. Native Windows/Linux GPU acceptance remains unrun; a configured lane is not a passing result. Windows CI is disabled.

### The performance-rules checklist

For GPU previews as a whole ([rules](../engineering/performance-rules.md#review-checklist)):

- **The original.** Read, hashed and decoded only through the verified prepared source: every boundary is derived on the GPU from the photograph's source the surface holds, uploaded once from the verified prepared source.
- **Full-frame allocations.** The surface slot's boundary texture, output, spatial planes and a chain's link intermediates within the 2 GiB GPU-preview budget, which refuses a slot past it and names the CPU path, a warp's grid of at most 2 MiB, and 32 MiB of upload staging a frame. Nothing is cloned that is shared.
- **Point queries, validation and no-op checks** render no frame: a plan is built from the compiled stack's descriptions in O(layers).
- **The owner thread** plans a tick's GPU plan in the draft's own answer and checks a boundary's size against its bound; all are arithmetic over descriptions, no frame work.
- **Desktop messages.** A tick drawn on the GPU sends no preview job and uploads no frame. `asset.state` and `history.list` are unchanged.
- **Timers, polls and subscriptions.** None added. The compile thread blocks on its queue, and a pass's completion is reported by a later submit with no poll; idle after a release measured 0.15% of one core.
- **Repeated work.** Compiled program sequences are cached and warmed when the stack changes; a spatial pass runs only when what it reads changed (5 compute passes for a Texture tick at 100%, none for Clarity).
- **editor-performance.** GPU previews change no core render; the drags above are the gesture's measurement.
- **Exactness.** Each program is qualified against its CPU unit on dense synthetic grids and on the corpus, within its class's limits, and the reference renderer keeps its exact bytes, which the core's exact-buffer tests hold.

### Verification

`verify --tier quick` and `--tier rendered` (with the private RAW manifest, 48 components) passed on the release editor above. `verify --tier timing` ran every component, but each timing component started above the 8.0 load threshold (10.0 to 10.6), so it reports every timing verdict as unreliable rather than as a pass or a miss. `measure` ran on its own twice, at loads of 3.0 to 4.3, with the same harness:

| `measure` target | Run 1 | Run 2 | Verdict |
| --- | ---: | ---: | --- |
| Launch to usable empty shell, p95 < 1 s warm | 1436 / 1472 ms | 1430 / 1489 ms | **Missed** |
| Uncached 24 MP JPEG to Fit preview, p95 < 750 ms | 783 / 827 ms | 784 / 824 ms | **Missed** |
| 24 MP working set ≤ 600 MiB, sampled RSS | 276 / 312 MiB | 284 / 317 MiB | Met |
| 60 MP peak ≤ 1 GiB, sampled RSS, one open | 419 / 452 MiB | 418 / 453 MiB | Met |
| Idle CPU < 1% of one core over 30 s | 1.6% | 0.49% | **Missed** in run 1, met in run 2 |

p50 / p95 over five launches (ten opens of the 24 MP JPEG). The launch figure is an upper bound: it runs from the spawn of a fresh background bundle with its copied 35 MB executable to the first captured frame, of which the editor's own startup to that frame is 0.2 s; the open figure runs from the request to the decoded raster. The idle window includes the open Performance section's sampler and moves between 0.5% and 1.6% of one core across runs.

## GPU qualification against the reference

The release gate ([GPU-first](../design/gpu-first.md#the-contract)): `cargo xtask gpu-qualification` over the whole [corpus](../../fixtures/preview/corpus.json), every stack rendered on the reference renderer and on the GPU at Fit, 33%, 50% and 100%, and every output kind the GPU renders held to its limit: the picture at rest (1,044 cells), the histogram and clipping counts, samples and export (261 stacks each), with no gap and no error. It runs as the release gate inside `verify --tier rendered`, and the last full run passed. The GPU's frames are the editor's own, drawn by the photo surface's own drawing on a headless device from the photograph's source held on the GPU as the editor holds it ([GPU previews](../design/gpu-preview.md#the-gpu-source)): the picture at rest, and the frame a drag draws. A measurement of pixels, not of time. By the [recorded default](../design/gpu-first.md#proposals-with-recorded-defaults), which gates the picture at rest against the reference and reports the picture in motion, the gate passed: the picture at rest within its limits on every cell, and the histogram, samples and export within theirs on every stack. The picture in motion is within its limits at 100% and past them on 462 of the 783 cells at Fit, 33% and 50%, against the picture at rest it settles to and against the reference alike. The limits stand under every reading. Dehaze's light is the per-frame light, computed by its light link from the whole stage at full resolution at every view ([GPU previews](../design/gpu-preview.md#spatial-programs)).

### Scope

- **What was compared.** At each view two GPU frames. The picture at rest: at Fit, 33% and 50% the editor's picture at rest in tiles, the whole output stage drawn at full resolution in tiles of 2048 px or smaller, each cut from the source, reduced to the view's size by the area-weighted average the reference is reduced by and quantized (1 to 240 tiles a stack, the most for Detail beside Presence, masked, on the 60 MP JPEG at Fit); and where the view draws the output at its own size or larger, as at Fit for the Presence fixture and the tight crop (40 cells), and at 100%, the view plan over the visible window, cut from the source at full scale. The frame a drag draws: the plan from the source over the boundary the surface derives, the source reduced to the view's proxy at Fit and below 100% and cut at 100%. Fit is the display-bounded proxy of an evidence run's 1440 × 900 window at 2× (1716 × 1508 bounds), 33% and 50% the displayed size of the whole output stage within the display bounds (4096 px a side, 8 MP), and 100% the visible region of the owner's largest window (1512 × 982 logical at 2×, panels closed) at full scale. The reference is the reference renderer's exact whole frame, as export renders it, reduced to the GPU frame's size by an area-weighted average of its linear light (`luxforge_reference::tolerance`), or at 100% its visible region, unreduced. Each comparison is the four statistics of the [preview error measure](../design/gpu-preview.md#the-preview-error-limit), each recipe held to its class's limits (pointwise 0.5 / 1.0 / 2.0 / ±0.25, spatial 1.0 / 2.5 / 5.0 / ±0.5).
- **Host and build.** The `Apple M4 Pro` adapter on Metal; the harness built in release from a clean working tree, the whole corpus in one run of `verify --tier rendered`, whose report is `target/integration-logs/verify-rendered-2/gpu-qualification/` in the worktree that built it. On one machine and driver the pixels are deterministic, so the host's load does not bear on them.
- **Corpus.** `fixtures/preview/corpus.json`: 42 recipes, 261 stacks (106 pointwise, 155 spatial), each at four views, over the generated 24 MP and 60 MP JPEGs, the zone plate, the zone plate with a lens identity and the Presence fixture, and the Z6 NEF, X100VI RAF and Air 2S DNG through the private RAW manifest, all eight verified and their SHA-256s unchanged after the run.
- **Memory.** The gate records no heaviest-slot figure. Measured over 250 of the corpus's stacks, every tile and every drag's slot fits the 2 GiB budget beside the source, the heaviest, a drag's slot at 100% of Detail then all three Presence fields on the Air 2S over its anchored 3970 × 2782 window, charging 1,466.4 MB with its light link, beside the Air 2S's 240 MB source.
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

- **Every cell is within its limits**, with no gap and no error, the largest a mean of 0.087, a worst block of 0.509 and a p99 of 0.963: the stack processed at full resolution and reduced after, as the reference is, leaves only the GPU's arithmetic and storage between the two, and Dehaze's light the whole stage's. The largest p99 of the spatial cells at every view is Detail beside Presence on the Z6 (0.955 to 0.963), whose light behind Detail in this run is the stand-in with Detail left out ([the light behind a spatial layer](#the-light-behind-a-spatial-layer) measures the exact light).
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

The frame a drag draws at 100% where the region's slot would pass the GPU-preview budget beside the source (`budget-reduced`, "Softer while dragging"), accepted subject to explicit qualification. `gpu-qualification` measures it on every 100% cell and reports it, never gated: the stack planned whole at the reduced stage of the view's area, `GpuView::Fit` at the region's displayed size as the desktop plans it, drawn on the GPU and magnified to the region bilinearly in linear light, standing in for the photo surface's sampler, which the headless surface does not run. Each cell records whether a drag there would draw it by the budget (`natural`) or it was drawn to qualify it (`forced`). Run at 100% alone, the `Apple M4 Pro` adapter, the corpus and the owner's RAW manifest; `artifacts/softer-qualification-1/` in the worktree that ran it.

| Class | Measured | Natural | Against the picture at rest: within / past | Largest | Against the reference: within / past | Largest | The reduced frame against the reference reduced: within / past | Largest |
| --- | ---: | ---: | --- | --- | --- | --- | --- | --- |
| pointwise | 106 | 0 | 21 / 85 | 14.0 / 26.5 / 61.1 / 13.2 | 21 / 85 | 14.0 / 26.5 / 61.1 / 13.2 | 45 / 61 | 8.45 / 16.4 / 31.7 / 7.51 |
| spatial | 171 | 0 | 26 / 145 | 16.7 / 38.5 / 62.5 / 12.7 | 26 / 145 | 16.7 / 38.6 / 62.5 / 12.7 | 80 / 91 | 21.9 / 46.2 / 68.2 / −28.1 |

- **No corpus cell draws it by the budget**: every region's slot fits the 2 GiB budget beside its source, as the design says. It is reached past the corpus: Detail beside Presence's three fields in a window about 1.4 times the owner's display's, or more than five masked Presence layers in the centred view ([at 100% and above](../design/gpu-preview.md#at-100-and-above)).
- **It is soft, as accepted.** Against the reference's region, the mean ΔE00 is 0.67 at the median pointwise cell and 0.58 at the median spatial cell, and about 12 at the 90th percentile of both, the largest on the generated detail and texture sources, where magnifying a frame of the view's size loses the fine detail the region holds. 230 of the 277 cells are past the motion limits. It is as far from the picture at rest it settles to as from the reference, because the picture at rest is within the at-rest limits on every one of these cells.
- **The reduced frame itself** is as far from the reference reduced to its size as the Fit motion frame above is: it processes a source reduced to the view's size, the same proxy.
- The at-rest gate at 100% in the same run: every cell within its limits.

### The GPU export against the reference export

`gpu-qualification --kind export`, the same host, corpus and manifest. Each stack is exported through the desktop's GPU tile worker on the `Apple M4 Pro` adapter and its codes compared over every pixel with the reference export's by the display limit of its class, then exported again through a second worker's own device. The full gate holds the export within its limit on all 261 stacks, with no gap and the same bytes twice, the largest 0.119 / 0.509 / 0.962 / 0.049 ([the full gate](#gpu-qualification-against-the-reference)).

- **No gap.** Every stack is drawn on the GPU, those that read Dehaze's light among them: the tile worker's runner computes each light once for the export, from the whole stage at full resolution a window of the source at a time, before the tiles that read it.

### The histogram and clipping counts

`gpu-qualification --kind histogram` over the corpus, the M4 Pro's Metal adapter, with the RAW manifest: a measurement of counts, not of time. The GPU's counts are the editor's own: each stack's tiles at full resolution as a committed job plans them (1 to 240 tiles of 2048 px or smaller), each over its window cut from the source, Dehaze's light computed on the GPU per frame, drawn by the photo surface's own drawing for their counts alone and counted by its histogram reduction; the same tiles are read back whole for the luminance histogram and the diagnostics, and their codes count to the GPU's counts exactly on every stack. The reference is the core's reducer over the reference renderer's exact whole frame. The limit: the earth mover's distance between the histograms within 0.25 code on each of R, G, B and luminance, and each clipping counter within 0.1% of the output pixel count.

- **Within the limit on all 261 stacks** of the full gate: the largest earth mover's distance 0.1296 codes (`r`, Basic under Dehaze with a crop, Presence fixture), the largest clipping difference 0.0313% of the output pixels, exact on 4 ([the full gate](#gpu-qualification-against-the-reference)).
- **Reported beside it, not gated**, over 250 of the stacks (`artifacts/gpu-first/task-004/gate-4`). The summed absolute bin difference is past 0.1% of the pixels on 89 stacks (34 of 106 pointwise, 55 of 144 spatial; 29 of 35 on the Presence fixture, 22 on the zone plate, 15 and 14 on the 60 and 24 MP JPEGs, 3 of 5 on the lens zone plate, 6 of 105 RAW stacks), the largest 23.6% on Basic under Dehaze with a crop on the Presence fixture, 7.4% for all three Presence fields there; binned at 2 codes 69 of them are still past 0.1%, and at 4 codes 50, since a flat patch one code across a bin's edge moves whole. Pixel by pixel, the frame differs from the reference frame by more than two codes on at most 0.455% of a stack's pixels (the perspective and lens warp over the lens zone plate's chirp, whose resampling also moves 31% of its pixels by one code) and on none of most stacks; the all-three-Presence-fields stacks on the Presence fixture reach 0.29%.
- **What it means.** The GPU's reduction counts its codes exactly ([basic and histogram](../design/basic-and-histogram.md#histogram-and-clipping-contract)). A summed difference counts a pixel whose code moved one step twice, so a share of a flat patch at a code's rounding edge moving by one code passes 0.1% of the pixels while the histogram moves by a hundredth of a code; the earth mover's distance measures how far it moved.

### Samples from GPU tiles

`gpu-qualification --kind sample`, the same host, corpus and manifest. Each stack is read at the 25 points of a 5 × 5 grid over its output stage by the desktop's GPU tile worker, as `render.sample` reads a pixel through it — one call, its points one session — against the reference frame's bytes there by the display limit of the stack's class (mean, p99 and signed mean ΔL\* over the 25 samples), and against the byte the picture at rest shows at each point at 100%, drawn by the photo surface over a 256 px window around it, which it must equal.

- **Within the limit on all 261 stacks** of the full gate, every point answered by the GPU and none by the reference: the largest mean 0.104, p99 1.006, \|signed mean ΔL\*\| 0.048 and single 1.006 ([the full gate](#gpu-qualification-against-the-reference)).
- **The byte on screen.** All 6,525 samples equal the picture at rest's byte at their pixel at 100%, which the anchored windows make so by construction.
- **Latency.** Not measured here; through three spatial segments on the Air 2S a GPU sample answers in 85.4 / 91.7 ms p50 / p95 warm ([a three-segment sample](#a-three-segment-sample)).

### Colour after Detail, before a resample

The corpus's three Detail recipes with colour between Detail and the geometry — `detail-curve-lens-perspective` on the three RAWs with their own lens profiles, `detail-curve-lens-perspective-jpeg` on the zone plate with a lens identity, and `detail-curve-straightened` on all seven sources — which the GPU draws, the colour running on Detail's output before the tail ([GPU previews](../design/gpu-preview.md#where-the-code-lives)). `gpu-qualification --recipes`, the same host, every kind: the 11 stacks at four views, 44 cells of the picture at rest, every one within the spatial limits.

| View | Cells | Within the limits | Largest: mean / worst block / p99 / \|ΔL\*\| |
| --- | ---: | ---: | --- |
| Fit | 11 | 11 | 0.035 / 0.116 / 0.693 / 0.006 |
| 33% | 11 | 11 | 0.044 / 0.108 / 0.692 / 0.006 |
| 50% | 11 | 11 | 0.087 / 0.213 / 0.695 / 0.006 |
| 100% | 11 | 11 | 0.049 / 0.203 / 0.696 / 0.006 |

- **The other kinds.** The 11 exports, histograms and sample grids are within their limits: the largest export 0.119 / 0.353 / 0.774 / 0.006, the largest histogram distance 0.0188 codes, every sample equal to the byte on screen.
- **In motion, not gated.** A drag's frame at 100% is the picture at rest's; at Fit, 33% and 50%, over the source reduced to the view, 21 of 33 cells are past the limits against the picture at rest, as the corpus's other stacks are ([the picture in motion](#the-picture-in-motion)).

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

## GPU-first latencies

The latencies of an ordinary edit with the GPU as the renderer of record, against the targets of the [GPU-first plan](../design/gpu-first.md). The picture at rest, its counts, export, painting at 100%, launch and the first picture on a cold shader cache are measured in [GPU throughput against the reference](#gpu-throughput-against-the-reference).

### Scope

- **Host.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal, a hidden 1440 × 900 window at 2× (2880 × 1800 physical) in a background-only bundle, warm filesystem cache. No other session built or tested on the host: no `rustc`, `cargo`, `clang` or `ld` process was running at the start of any run. The one-minute load at the start of each run is given with its figure; every run quoted started below the 8.0 threshold. A reference-renderer run raises the load itself, so each was started only once the load had fallen below 3.5.
- **Build.** Release, `--locked`. Two executables of the same source: one from `cargo build --release --locked -p luxforge-app -p xtask`, which the drag runs and the timing tier use, and one from the whole workspace's build that `cargo xtask develop --background` makes, whose feature unification differs, which the evidence-script probes (samples, Compare and history, idle memory) and the white-balance and Dehaze runs use. Each report records its own `binary_sha256`.
- **Sources.** The Air 2S DNG of the private manifest (5464 × 3640, 19.9 MP; the design's "16 MP" is this file), the generated 24 MP and 60 MP JPEGs. The three RAWs of the manifest hashed the same before and after every run.
- **Stacks.** *The drag stack*: Detail (sharpening 60, luminance 40, colour 40), Presence's three fields at +100 and a full Basic layer, every field non-neutral (`editor-latency --detail --presence --basic`), the measured gesture a Basic exposure drag, which runs before Presence, so every tick runs the whole chain and computes Dehaze's light again. *The masked stack*, the design's freeze stack: Detail, a global Presence of Texture 25 and Clarity 20, and three masks, a brush and two radials, each holding a masked exposure and a masked Presence of Clarity 50 and Texture 40 (`editor-latency --mode paint --masks 3 --mask-presence --presence --detail`), measured as a painted stroke. *The three-segment stack* of the samples: Detail as above, Presence's three fields at +100 and a radial's masked Presence of Clarity 50 and Texture 40, the Air 2S's lens profile after them.
- **Figures.** Milliseconds, nearest rank. A GPU frame is timed to the surface's first draw of its plan, a CPU frame to its hand-off ([development](../engineering/development.md)); neither is scanout, and on the 120 Hz display a frame is 8.3 ms. Where fewer than 30 samples were taken the median and range are given and no p95.

### A three-segment sample

`render.sample` through the three-segment stack on the Air 2S, through the desktop's GPU tile worker, each call an `api` step of an evidence script launched with `cargo xtask develop --background --hidden-window --evidence-dir` and timed from the step to its answer (`script_step` to `script_step_settled`, `host_answered`), 31 points at random over the stage, after the picture at rest and the warm-up had finished. Every answer named `renderer` `{record: "gpu"}`.

| Run | Load | Samples | p50 / p95 | Range |
| --- | ---: | ---: | ---: | ---: |
| Warm: the shared bundle, after the launch's first sample | 2.35 | 30 | 85.4 / 91.7 | 70.9 to 93.9 |
| The launch's first sample, the shared bundle | 2.35 | 1 | 140.9 | |
| The launch's first sample on a fresh bundle's empty Metal cache (`LUXFORGE_BACKGROUND_BUNDLE_SUFFIX`) | 3.81 to 4.36 | 3 launches | 158.3 | 156.5 to 164.1 |
| The two after it on those fresh bundles | 3.81 to 4.36 | 6 | | 89.6 to 97.6 |

- **The target is met**: warm p95 91.7 ms against S6.7's 100 ms. No read walks a stage on the owner or a point worker. The first read of a launch compiles the stack's programs on the tile worker's device, 50 to 80 ms more.

### Compare and a history return

An evidence script over the Air 2S with Detail, Presence's three fields and a Basic exposure of +0.3, five of each step, `wait` steps of 2.5 s between them; load 3.54.

| Step | Frame presented | Picture at rest drawn | Counts |
| --- | ---: | ---: | ---: |
| Compare on (the reference renderer's frames of both sides, by design) | 592.5 to 652.7 | | with the frame |
| Compare off (its retained frames restored) | 19.3 to 21.1 | 1,026 to 1,043 | 1,092 to 1,108 |
| History: an earlier entry (Detail alone) | 8.8 to 9.0 | 76 | 84 |
| History: back to the current entry | 8.8 to 9.0 | 1,040 to 1,064 | 1,105 to 1,131 |

- The photograph returns within one or two frames and sharpens about a second later.

### A RAW white balance

The Air 2S's Temperature, `--action set-raw --parameter temperature`, no other layers. The drag drew 30 of 30 inputs on the GPU at **8.5 / 9.0 ms** (load 2.46). Its releases, 30 commits of one input each (load 2.50): the redeveloped picture presented 133.8 / 141.8 ms after the release, its picture at rest 255.6 / 276.9 ms, its counts 263.4 / 285.5 ms. After each release the next gesture's first tick waits for the redeveloped source's boundary (`surface-pending`) and is drawn 8.7 / 9.2 ms after its input. These figures are paired from the runs' `events.jsonl` exactly as `editor-latency` pairs a GPU tick, a pairing that reproduces the tool's own 9.0 / 9.2 ms on the drag stack's Fit run.

### A colour drag under Dehaze

A Basic exposure drag under Presence's three fields at +100 and a full Basic layer (`--basic --presence`), so Dehaze's light is computed on the GPU for every tick from the whole stage at full resolution; 30 inputs each, all on the GPU:

| Source | View | Load | Input to drawn p50 / p95 | `draft.set` round trip p50 / p95 |
| --- | --- | ---: | ---: | ---: |
| 24 MP JPEG | Fit | 2.98 | 8.8 / 9.1 | 0.2 / 0.2 |
| 24 MP JPEG | 100% | 3.24 | 8.7 / 9.1 | 0.2 / 0.3 |
| 60 MP JPEG | Fit | 3.86 | 9.0 / 9.4 | 0.2 / 0.2 |
| 60 MP JPEG | 100% | 4.62 | 9.0 / 9.3 | 0.2 / 0.2 |

- Every tick is drawn at the first redraw after its input, so the light's per-tick cost fits within a 120 Hz frame at 60 MP; the tools time frames, not GPU passes, so its own duration is not separated.

### Idle memory

In the performance scenario the footprint and RSS grow about 20 MB with every idle capture 5 s apart. The 60 MP JPEG with Detail, Presence's three fields and a full Basic layer, a release editor in the background bundle, sampled every 5 s with `ps` (RSS) and `footprint` (`phys_footprint`):

- **With no captures** (an ordinary `develop --background` launch on a copy of the stack's catalog, no evidence directory, the Performance section open, load 4.38): once the picture at rest had settled, RSS held at 870.9 to 871.0 MiB for two minutes and the footprint moved between 1.970 and 2.010 GB with no trend; the process used 1.31 s of CPU in 117 s, 1.1% of one core, the Performance section's sampler included.
- **With a capture every 5 s** (an evidence script of 26 `wait` steps of 5 s, load 2.11): RSS rose by 20,320 to 20,480 KiB at every capture, which is one 2880 × 1800 window readback of four bytes a pixel (20.7 MB), and fell back by 180 to 200 MiB twice in the two minutes, as the allocator returned the readbacks' pages.
- So the growth is the evidence capture's readback, held until the allocator returns it, not a leak of the editor at idle.

### The timing tier

A `verify --tier timing` run whose own `check` raises the one-minute load past the 8.0 threshold reports every timing verdict as unreliable rather than as a pass or a miss, so its timing components are run on their own once the load has fallen below 3.5, at the sample counts the targets name (`measure` at five launches a workload):

| Provisional target | Measured alone | Load | Verdict |
| --- | --- | ---: | --- |
| Warm 24 MP slider-to-presented-frame p95 < 16 ms | 8.2 / 9.0 ms, 30 inputs, all on the GPU | 3.45 | **Pass** |
| Settled exact histogram p95 < 200 ms after the final input, 24 MP | 67.7 / 76.0 ms, 30 commits (exposure alone) | 3.50 | **Pass** |
| Burst ≥ 30 presented frames a second; staleness p95 ≤ 50 ms | 109.8 frames a second; 8.0 / 8.4 ms over 339 frames | 3.26 | **Pass** |
| 24 MP working set ≤ 600 MiB, sampled RSS | 336.4 / 436.2 MiB | 2.99 | **Pass** |
| 60 MP peak ≤ 1 GiB, sampled RSS, one open | 498.3 / 723.1 MiB | 2.99 | **Pass** |
| Idle CPU < 1% of one core over 30 s | 1.43%, one window, the Performance section open | 2.99 | **Miss** (one window) |

Launch and the uncached 24 MP open are in [launch and the uncached 24 MP open](#launch-and-the-uncached-24-mp-open).

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

## GPU throughput against the reference

The GPU's picture at rest, its counts and its export, each against the same build's reference renderer (`--reference-renderer`, which launches with `--no-gpu-render`, or `reference: true` for an export), back to back. The release gate passes on the whole corpus with the exact light ([the light behind a spatial layer](#the-light-behind-a-spatial-layer)).

### Scope

- **Host.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal, a hidden 1440 × 900 window at 2× in a background-only bundle, warm filesystem cache; no low-power mode. No timed run's window overlaps a host sleep (checked against `pmset`'s sleep and wake records). Runs on battery and on mains power agree within their spread.
- **Quiet host.** Before every timed run a gate waited until no `cargo`, `rustc`, `clang`, `ld` or Clippy process of another worktree was running and the one-minute load was below 4.0; every run quoted started between 2.5 and 4.0. A reference run raises the load itself (to 7 to 10 by its end), so the run after it waited for the load to fall.
- **Build.** Release, `--locked`. `cargo build --release --locked -p luxforge-app -p xtask` makes the 52.0 MB executable every `editor-latency` and `measure` run uses (`--binary`); `cargo xtask develop --background`'s build of `luxforge-app` and `luxforge-cli` makes the one the evidence-script probes use (exports, the cold shader cache).
- **Sources.** The Air 2S DNG (5464 × 3640), the X100VI RAF (7728 × 5152), the generated 24 MP and 60 MP JPEGs, each hashed the same before and after the runs.
- **Stacks.** *The drag stack* and *the masked stack* as [GPU-first latencies](#gpu-first-latencies) defines them. *The three-segment stack*: Detail, Presence's three fields at +100 and a masked Presence on a linear mask (`editor-latency --mode commit --detail --presence --mask --action set-presence --parameter clarity`), each commit a new masked Clarity, over the Air 2S as imported. *The heavy export stack*: Detail, Presence's three fields and a radial's masked Presence; *the masked export stack*: Detail, a global Presence of Texture 25 and Clarity 20, and three radial masks each holding a masked exposure of +0.3 and a masked Presence of Clarity 50 and Texture 40, radials where the masked stack has a brush.
- **Figures.** Milliseconds, nearest rank, p50 / p95 over 30 samples unless a count is given. *Rest* runs from the release to the surface's first draw of the picture at rest (`gpu_rest_drawn`), *counts* to the GPU's settled histogram (`analysis_adopted`); the reference renderer's frame and its report arrive together, the counts the tool reports. Commit runs time each commit after the previous one's counts have arrived.

### The picture at rest and the counts after a commit

| Row | GPU rest | GPU counts | Reference frame and counts | Load (GPU, reference) | Tiles drawn |
| --- | ---: | ---: | ---: | --- | --- |
| 24 MP JPEG, the drag stack, GPU first | 210.1 / 231.9 | 242.4 / 263.6 | 644.4 / 694.6 | 3.67, 3.20 | 2 sweeps of 6 tiles of 2048 px |
| The same, reference first | 201.6 / 209.9 | 225.7 / 242.3 | 665.3 / 682.2 | 3.99, 3.27 | the same |
| Air 2S, the drag stack, reference first | 226.2 / 231.8 | 263.7 / 268.5 | 1,136.5 / 1,188.0 | 3.30, 2.86 | 2 sweeps of 6 tiles of 2048 px |
| The same, GPU first | 226.9 / 230.5 | 264.1 / 266.2 | 1,152.9 / 1,311.0 | 3.08, 3.38 | the same |
| 60 MP JPEG, the drag stack, GPU first | 565.7 / 577.6 | 601.5 / 613.8 | 1,923.3 / 2,037.3 (28 commits) | 3.36, 3.36 | 2 sweeps of 15 tiles of 2048 px |
| The same, reference first | 564.1 / 568.1 | 599.9 / 604.0 | 1,763.6 / 1,952.0 (20) | 3.10, 3.52 | the same |
| The same, reference first again | 574.3 / 585.8 | 610.6 / 621.7 | 2,092.3 / 2,348.0 (20) | 3.99, 3.92 | the same |
| X100VI RAF (40 MP), the drag stack, reference first | 597.7 / 614.2 | 615.8 / 646.6 | 2,432.2 / 2,564.5 (20) | 3.85, 3.73 | 2 sweeps of 12 tiles of 2048 px |
| Air 2S, the three-segment stack, GPU first | 301.9 / 333.5 | 309.4 / 341.2 | 959.7 / 1,022.6 | 2.92, 2.84 | 3 sweeps of 24 tiles of 1024 px |
| The same, reference first | 297.7 / 317.3 | 305.6 / 326.1 | 976.4 / 1,053.8 | 3.97, 3.87 | the same |
| Air 2S, the masked stack's stroke release, one a launch | 672.8 to 700.0 (5 launches) | 706.2 to 732.4 | 1,301.8 to 1,420.5 (4 launches) | 2.51 to 3.99, 2.98 to 3.91 | 4 sweeps, 42 tiles |

- **Every row's picture at rest and counts land sooner than the reference renderer's frame and report, at p50 and p95**: by 2.6 to 2.9 times at 24 MP, 4.3 to 4.9 times on the Air 2S, 2.9 to 3.4 times at 60 MP, 3.9 times on the X100VI, 3.1 times over the three segments and about 2 times after the masked stroke. The reference's own figures moved between runs more than the GPU's (at 60 MP 1.76 to 2.09 s at p50 in three runs); the tool's 60 s deadline let the first reference run at 60 MP finish 28 of 30 commits, paired from its events, and the others were run at 20.
- **Attribution, from `gpu_rest` and `gpu_rest_drawn`.** Each picture at rest is planned in tiles of 2048 px wherever the share allows. Behind Detail every Dehaze stack is drawn in at least two sweeps, the first writing the stage texture its exact light is reduced from (`lights_encoded` 1, `lights_restored` 5 to 8, `retirement_waits` 0 to 1 a picture; no light sweep was needed on any row: the stage textures fitted). The tiles' summed GPU spans (an upper bound, the spans overlapping) were 0.23 s at 24 MP, 0.36 s on the Air 2S, 0.39 to 0.41 s over the three segments, 0.64 s on the X100VI, 0.85 to 0.87 s after the masked stroke and 1.04 to 1.06 s at 60 MP, the slowest tile 28 to 61 ms; at 60 MP the picture at rest arrives in about half its tiles' summed spans, which overlap. After the masked stroke the rest names 330 tiles of 256 px for its chained plan and draws 4 sweeps of 42 tiles, with 26 refits and 9 rebinds; the GPU-preview peak was 2,146.5 MB of the 2,147.5 MB budget.
- **The committed frame** is the GPU's view plan at once: 9.5 to 10.0 ms at p50 and 9.7 to 17.2 ms at p95 on every GPU row.
- **The 60 MP RAW drag stack**, whose stage textures do not fit, so its light is computed by a light sweep: no 60 MP RAW is in the corpus, so it was not measured in the editor (the X100VI's 40 MP stack fitted its stage textures and drew no light sweep). Its indication on the headless surface (`gpu_rest_the_60_mp_raw_drag_stacks_light_indication`, release, over 9504 × 6336 synthetic planes, the picture at rest drawn once to warm, its kept light let go, and timed again; two runs, loads 3.16 and 4.03, an indication and not a timing): as the editor plans it, 20 light-sweep tiles of Detail and 70 chained, 1.63 and 1.65 s, the tiles' spans summing 5.40 and 5.41 s; staged at 1024 px 0.99 s; the reference renderer's whole frame 2.57 and 2.55 s.

### A developed photograph's catalog tiers

Both tiers (grid 512 px, large 2048 px) of the corpus's Z 6 and Air 2S under a Basic layer and then a Presence layer, drawn by the GPU tile worker and by the reference, each from its own preparation of the original: `a_gpu_tier_of_a_raw_is_within_the_display_limit_of_the_reference_tier` (release, `--ignored`), three runs at loads 3.18 to 3.62, each run drawing the GPU's tiers first, so the order was not reversed. An indication from a test, not the catalog's own lane under the editor.

| Source and layer | GPU, three runs | Reference, three runs |
| --- | ---: | ---: |
| Z 6, Basic | 333, 241, 252 | 423, 415, 421 |
| Z 6, then Presence | 552, 341, 399 | 802, 798, 828 |
| Air 2S, Basic | 246, 235, 241 | 402, 406, 406 |
| Air 2S, then Presence | 346, 334, 338 | 705, 681, 711 |

- The GPU draws both tiers sooner than the reference on every row in every run, the Z 6 with Presence in 0.34 to 0.55 s against 0.80 to 0.83 s. Each tier is within its class's display limit of the reference's (largest mean 0.0195, worst block 0.094, p99 0.615).

### Export

`export.write` through the evidence `export` step, five GPU exports and five reference exports (`reference: true`) alternating, after a `gpu_warmed` step; the step is timed from its request to its end, which the desktop reads through `job.wait`. Beside it, the activity board's `duration_ms` for exports of 250 ms or more. Every GPU export named `{record: "gpu"}`, every reference export `{record: "reference", reason: "requested"}`.

| Source and stack | GPU: median, range (5) | Reference: median, range (5) | Load | GPU tiles an export; tile worker's peak |
| --- | ---: | ---: | ---: | --- |
| 24 MP JPEG, trivial (Basic exposure +0.3) | 101, 100 to 110 | 85, 84 to 92 | 2.90 | |
| 24 MP JPEG, heavy | 275, 269 to 374 (board 258, 251 to 359) | 784, 767 to 819 (board 768, 751 to 801) | 2.90 | |
| 60 MP JPEG, trivial | 190, 187 to 199 | 175, 167 to 185 | 3.41 | |
| 60 MP JPEG, heavy | 629, 622 to 656 (board 613, 606 to 637) | 2,016, 1,944 to 2,247 (board 1,998, 1,928 to 2,222) | 3.41 | |
| Air 2S, the masked export stack | 457, 451 to 522 (board 441, 433 to 503) | 1,177, 1,149 to 1,191 (board 1,153, 1,132 to 1,174) | 3.31 | 24; 2,112.9 MB |
| The same, a second launch | (board 467, 451 to 494) | (board 1,170, 1,131 to 1,193) | 3.43 | 24; 2,112.9 MB |

- **A heavy GPU export is 2.9 to 3.3 times faster than the reference export** at 24 and 60 MP and 2.5 times over the masked stack.
- **A trivial GPU export misses by 15 to 16 ms**: 101 against 85 ms at 24 MP and 190 against 175 ms at 60 MP, every GPU export slower than every reference export. Attributed below ([the trivial export's first band](#the-trivial-exports-first-band)).
- The tile worker's peak was 2.11 GB of its 2 GiB (2,147.5 MB) budget on the masked stack.

### The trivial export's first band

Timed in the process, through the owner's export lane as the desktop starts it (`trivial_export_timing_gpu_against_reference`, release, seven alternating samples after one discarded export of each, the generated fixtures with Basic exposure +0.3, the M4 Pro at one-minute loads of 5 to 9): **75 against 57 ms at 24 MP and 158 against 137 ms at 60 MP**. The desktop's step timing above reads the end on its frame clock, about 8.3 ms a frame, so it shows the same gap coarser.

- **Where the gap is.** The reference renders the pointwise stack across the pool in 7 to 8 ms (18 ms at 60 MP) and then encodes. The GPU's encoder starts at once and waits for the stream's first band, a row of 2048 px tiles: 26 to 28 ms at 24 MP and 40 to 52 ms at 60 MP. After it, the encoder never waits again. The gap is that first band's latency less the reference's render.
- **What the first band costs.** On the tile worker's thread, timed tile by tile: the band's window of the source uploaded, 4.7 ms for 6000 × 2048 codes; the first tile's wait for the device, 10 to 11 ms, where a later tile of the band waits 1.3 to 2.6 ms; and each tile's readback and copy into the band, about 2.2 ms. A fresh slot does not explain the first tile's wait: keeping the slot across exports left it unchanged.
- **Tried and not adopted:** 512 px tiles (134 ms at 24 MP: per-tile costs dominate) and 1024 px tiles (93 against 84 ms in the desktop's timing, 184 against 168 ms at 60 MP); a first row of 512 or 1024 rows under 2048 px tiles (at most 3 ms: the second band is then late, the worker taking about 21 ms a band against the encoder's 24); reading a band's last tiles back and sending it before the next band's window is uploaded (73 and 151 ms, but a stream drawn without an encoder, which bounds a heavy export, slower by 7 ms for Detail at 24 MP and 12 ms for the 60 MP drag stack, the device idle during the upload); and copying the mapped readback straight into the band (no change).
- **What is left.** The window's upload and the device's first-tile latency, each paid per band on the worker's one thread. Closing the gap needs the next band's window uploaded beside the current band's tiles, or a band's window textures kept for the next band of its shape, both changes to what the runner holds at once ([export](../design/export.md)); not built.

### Drag ticks

30 drained inputs each, every one drawn on the GPU, none held.

| Drag | View | p50 / p95 | Load |
| --- | --- | ---: | ---: |
| Air 2S, Basic exposure over the drag stack | Fit | 9.1 / 9.4 | 3.42 |
| The same | 33% | 9.0 / 9.1 | 3.62 |
| The same | 100% | 9.0 / 9.3 | 3.37 |
| 60 MP JPEG, Basic exposure under Presence's three fields (Dehaze) | 100% | 9.0 / 9.3 | 3.45 |
| Air 2S, a masked Presence Clarity drag over Detail and Presence (`--mask --action set-presence --parameter clarity`) | Fit | 8.5 / 8.7, worst 26.3 | 3.25 |

### Painting at 100% over the masked stack

240 positions over the masked stack at 100% on the Air 2S, launch by launch (`editor-latency --warm 4000` in most, so the 100% view's warm-up and picture at rest are done before the stroke; one-minute load 2.95 to 6.91): **8.2 / 23.6 ms** (no wait), **8.2 / 23.7**, **7.9 / 10.5**, **8.0 / 23.8** and **8.0 / 23.3 ms** (waited). **A miss at p95.** At Fit 60 positions drew at 7.8 to 8.0 / 8.6 to 9.0 ms in five launches; the reference renderer draws the same stroke at Fit at 102.6 / 115.3 ms over the 65 positions it presented.

- **The same links on every tick, no refit and no rebind** (`surface_frame_drawn`'s `evaluation`, joined to each input by its draft revision): 5 links run, 52 spatial passes, a window of 6.39 MP and 31.9 M link texels. `spatial_passes` counts a pass dispatched over nothing too, so the 52 passes are not the same work on every tick (below).
- **The tail is the GPU time of the ticks that do spatial work.** About two thirds of the frames do no spatial work at all: the stroke changes the plan only where a new dab lands. The frames that do write 20 to 60 M texels in their spatial passes (p50 39 to 41 M, the largest 225 M), several times the 6.39 MP window: three masked Presence links of 13 passes each, every pass over the change grown by the links' chained halos. Every slow position of two launches, 29 and 24 of 240, was such a frame, 18 and 13 of them in the third of the frames with the most spatial work. The surface's pass clock, an upper bound with up to a frame's wait for the next submit in it, reads 10 to 33 ms for those ticks against about 1 ms for the others. A tick is presented 8, 15 or 24 ms after its input, one, two or three frames of the 120 Hz display, and nothing in between: a tick whose GPU work passes one or two frames is presented on the second or third. The slow ticks come in runs of consecutive positions. A smaller window draws the same stroke within the target: at `--window 1000x700`, a 3.53 MP window, **7.7 / 8.8 ms** (10 of 240 past 12 ms, load 2.98).
- **Not whole re-evaluations, a refill, the picture at rest, or time since the zoom.** Every tick of the stroke runs incrementally, the slow ones among them: none runs its links over the whole window, and the texels a tick's links reach and the coverage worker's completions are spread the same over slow and fast ticks. No tick refills a link over the whole window (`refilled` 0), a masked link's planes holding what its mask needs from the first tick on; the picture at rest's tiles, at 100% drawn for their counts alone, are not handed while a gesture is open; and with the stroke 4.3 s after the zoom the slow positions still come in runs. Without the warm wait, the first ticks of a stroke run while the 100% view's warm-up compiles and take 51 to 58 ms. Meeting 16 ms at p95 here needs less spatial work a tick at 100%, which is not designed.

### Launch and the uncached 24 MP open

`measure` at five launches a workload, three runs. Each launch is split at the first RSS sample above 1 MiB (the harness's copy of the executable into a fresh bundle and macOS's first check of it), the log clock's zero (the editor's `startup` event, from the launch figure less the capture's elapsed time) and the first `frame_captured`. p50 of the five launches in each run:

| Run | Load | Launch to empty shell | Spawn to first RSS | First RSS to `startup` | `startup` to capture | 24 MP open to raster | 24 MP `decoded` after `startup` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 3.54 | 2,071 | 807 | 1,040 | 273 | 1,214 | 254 |
| 2 | 3.07 | 2,107 | 806 | 1,035 | 259 | 1,211 | 249 |
| 3 | 3.85 | 2,069 | 760 | 1,038 | 271 | 1,253 | 256 |

- **Launch misses the provisional p95 under 1 s, and the uncached open p95 under 750 ms.** About 1.0 s of every launch and of every open's `open_to_raster_ms` lies between the process's first RSS sample and its `startup` event (`open_to_raster_ms` runs from the editor's creation, before the log clock starts); 0.76 to 0.81 s comes before the first RSS sample, the harness copying the 52.0 MB executable into a fresh bundle and macOS checking it on first run. The editor's launch figure is the harness's upper bound, not a foreground activation.
- **The open's first picture** is the GPU's view plan when its programs are warm (`decoded` `reduced: true`, `preview_displayed` naming `gpu`, a render of about 10 ms) 249 to 256 ms after `startup`; the picture at rest follows in 6 tiles 325 to 355 ms after `startup`. The capture `measure` takes waits for the picture at rest and its dissolve, so `startup` to capture at 24 MP is 488 to 519 ms.
- **Idle CPU after an open**: 0.0% of one core over 30 s in all three runs.

### The first picture on a cold shader cache

The three-segment probe on the Air 2S, three launches each with a new `LUXFORGE_BACKGROUND_BUNDLE_SUFFIX`, so the bundle's Metal cache starts empty, then one on the shared, warm bundle; milliseconds from `startup`, the Air 2S opened with no edits, loads 2.51 to 3.27:

| Run | Reference frame drawn | GPU frame drawn | GPU after the reference | First warm-up |
| --- | ---: | ---: | ---: | --- |
| Fresh bundle 1 | 1,328 | 2,165 | 837 | 2,087 ms, 21 sequences, its picture's own 577 ms |
| Fresh bundle 2 | 1,404 | 2,199 | 794 | 679 ms, 6 sequences, 380 ms |
| Fresh bundle 3 | 1,366 | 2,168 | 802 | 686 ms, 6 sequences, 384 ms |
| Shared bundle | 612 | 723 | 111 | 42 ms, 14 sequences, 3 ms |

- The GPU's first frame on a cold cache comes 0.79 to 0.84 s after the reference's. The second and third fresh bundles compiled 6 sequences where the first compiled 21, so the system's compiler cache outside the bundle, which a new suffix does not clear, served the rest; only the first launch is a cold compile.

### The timing tier

`verify --tier timing --binary` ran once and passed, but its own `check` left the one-minute load at 19.5 when the timing components started, past the 8.0 threshold, so it reports every timing verdict as unreliable. Its figures agree with the runs above: the 24 MP slider 8.66 ms at p95, the settled histogram 76.2 ms, a burst at 110 frames a second with staleness 8.6 ms at p95, the 24 MP working set 332.6 MiB, the 60 MP peak 507.3 MiB, idle 0.03% of one core, launch 2,152 ms and the uncached 24 MP open 1,278 ms at p95. By the quiet-host runs above and [those of the components alone](#the-timing-tier), the provisional slider, histogram, burst, memory and idle-after-an-open targets pass, and launch (p95 under 1 s) and the uncached open (p95 under 750 ms) miss.

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
# The 60 MP RAW drag stack's light indication on the headless surface.
cargo test --release -p luxforge-app -- --ignored gpu_rest_the_60_mp_raw_drag_stacks_light_indication --nocapture
[LUXFORGE_BACKGROUND_BUNDLE_SUFFIX=NEW] cargo xtask develop --background --hidden-window \
  --evidence-dir NEW_DIR --evidence-script SCRIPT.json --open /ABSOLUTE/SOURCE
```

The picture at rest is read from the run's `app/events.jsonl` (`gpu_rest`, `gpu_rest_drawn`); a launch's first RSS sample from `measurements.json`'s `rss_samples`.

## Export settings

What each [export setting](../design/export-settings.md) costs, GPU against reference, measured by `export_settings_measurement` (`crates/luxforge-app/src/app/gpu_tiles_worker_tests.rs`) in release.

- **Host.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal, shared with other sessions' builds and runs. One-minute load 5.9 and 5.5 at the start of the 24 MP and 60 MP timing runs, rising to about 10 while they ran, and 9.6 at the start of the Z 6's, above `verify`'s threshold of 8.0. The GPU and reference exports alternate, so the load weighs on both alike.
- **Build.** The `--locked` release test binary, the same executable the [qualification](../design/export-settings.md#qualification-and-measurement) ran.
- **Scope.** Through the catalog owner's export lane as the desktop starts it, with the desktop's GPU tile worker; the original as it is (the generated `24mp.jpg` and `60mp.jpg`, and the Nikon Z 6 NEF, 4024 × 6048) with Basic exposure +0.3, a light stack, so the figures are the settings' own cost more than the render's. Each configuration on a fresh owner and worker, one export of each renderer as a warm-up, then five of each, alternating; medians of five. Time is `export.write` to the job's end, read through `job.wait`; phases are the spans between the job's progress messages. Memory is the process's lifetime peak footprint above its footprint before the export, in a process that prepared the photograph and exported once on one renderer: what an export adds to a process that has not exported. A GPU export's figure includes opening the tile worker's device and its charges, which on the M4's unified memory are in the footprint. One process per configuration and renderer, so one sample each. The generated JPEGs are flat patterns, so their file sizes are not those of photographs; the Z 6's are.

Medians in milliseconds, GPU / reference; then peak additional memory in MB, GPU / reference:

| Configuration | 24 MP JPEG | 60 MP JPEG | Z 6 NEF |
| --- | --- | --- | --- |
| JPEG q90 4:4:4, full size | 73 / 56; 602 / 117 | 162 / 140; 745 / 263 | 122 / 350; 843 / 98 |
| The same, optimised | 488 / 477; 780 / 400 | 1,227 / 1,182; 1,216 / 975 | 805 / 981; 963 / 342 |
| Web | 95 / 67; 911 / 249 | 159 / 109; 1,514 / 499 | 177 / 332; 1,300 / 204 |
| Print | 307 / 95; 923 / 112 | 893 / 239; 1,512 / 259 | 308 / 362; 1,268 / 101 |
| Master (16-bit TIFF, Deflate) | 386 / 197; 923 / 214 | 1,034 / 487; 1,524 / 497 | 3,009 / 3,080; 1,317 / 342 |
| Master, uncompressed | 316 / 122; 921 / 219 | 819 / 204; 1,536 / 860 | 290 / 314; 1,273 / 342 |
| WebP q80, full size | 514 / 488; 651 / 151 | 1,308 / 1,493; 863 / 366 | 948 / 1,104; 937 / 110 |
| The same, optimised | 1,161 / 1,127; 651 / 329 | 2,876 / 2,794; 1,076 / 785 | 1,785 / 1,936; 1,001 / 281 |
| JPEG q90 4:4:4, limited | 366 / 387; 651 / 117 | 917 / 984; 862 / 258 | 524 / 678; 909 / 99 |
| JPEG q90 4:4:4, long edge 2048 | 74 / 39; 921 / 241 | 204 / 103; 1,518 / 499 | 113 / 259; 1,307 / 192 |

- **Optimise.** A full-size JPEG export is 6.6 to 7.6 times slower on the GPU and 2.8 to 8.5 times on the reference, whose render of the Z 6 is a larger share. The encode itself, `encoding` and `optimising` together against the baseline's `encoding`, is 9.7 to 10.6 times slower. It adds 283 MB to the reference's peak at 24 MP and 712 MB at 60 MP, against `coefficient_bytes`' 288 and 723 MB, and 244 MB on the Z 6. The Z 6's file is 16% smaller (3,244 to 2,718 KiB). For a TIFF, Deflate takes the Z 6's Master from about 0.3 s to 3.0 s on both renderers, and its file from 139 to 109 MiB.
- **Limit file size.** Each limit is halfway between the unlimited files at qualities 10 and 90 on the GPU. The search took 7 encodes on the GPU and 8 on the reference for the generated JPEGs, whose two renderers' files differ by a few bytes, choosing qualities 64 and 65 at 24 MP and 62 and 65 at 60 MP; 8 on both for the Z 6, at quality 80. A search takes 4.3 to 5.7 times a single unlimited export on the GPU, and 1.9 to 7.0 times on the reference, whose share of rendering varies.
- **Resampling's share.** On the reference, the `resampling` phase of a long edge of 2048 is 17 of 39 ms at 24 MP, 44 of 103 ms at 60 MP and 17 of 259 ms on the Z 6; Web's, which includes its sharpening, is 22, 41 and 25 ms. Print's, a full-size sharpening, is 38, 97 and 42 ms. On the GPU the resample is the stream's last sweep, inside `encoding`: the resized export costs about what the full-size baseline does (74 against 73 ms at 24 MP, 204 against 162 ms at 60 MP, 113 against 122 ms on the Z 6).
- **The held output stage.** Print and Master on the GPU go through the held `Rgba32Float` stage at full size: on the generated JPEGs 3.2 to 3.7 times the reference's Print and 2.0 to 2.1 times its Master, whose render of a light stack takes 8 to 19 ms. On the Z 6 they are about as fast. The stage is the tile worker's peak of 1,313 MB at 60 MP, against 324 MB for the streamed baseline.
- **Against 1 GiB of process memory**, which an export may exceed: the reference's process peaks at 487 to 1,171 MiB, Optimise's being the highest; the GPU's at 952 to 1,706 MiB, of which the tile worker's charges are 309 to 1,253 MiB of GPU memory in the same footprint.

Evidence: `private/evidence/export-settings/task-008-measure/` in the owner's checkout (`timing.jsonl`, `memory.jsonl`, `load.log`, `table.md`).

```sh
cargo test --release --locked -p luxforge-app --bin luxforge --no-run
# One process per source for timing; with LUXFORGE_EXPORT_MEMORY=gpu|reference, one per
# configuration and renderer for memory (LUXFORGE_EXPORT_LIMIT_BYTES gives the limit search its limit).
LUXFORGE_EXPORT_SOURCES=24mp|60mp|raw LUXFORGE_EXPORT_RAW=/ABSOLUTE/Z6.NEF LUXFORGE_EXPORT_OUTPUT=FILE \
  [LUXFORGE_EXPORT_CONFIGURATIONS=NAME,...] [LUXFORGE_EXPORT_MEMORY=gpu|reference] \
  target/release/deps/luxforge-HASH --ignored --exact app::gpu_tiles_worker_tests::export_settings_measurement --nocapture
```

## The per-frame light's reduction factor

The factor f the per-frame estimate twin of [GPU-first](../design/gpu-first.md)'s stage 3 needs. The twin is to compute Dehaze's atmospheric light every frame from the stack compiled at a block stage, the content stage reduced by f per side, f dividing 16: each f × f cell's mean through the colour run, each 16 px block's cells averaged, and the light selected from the blocks as Dehaze prepares it. The reference averages each 16 px block of the colour run's output instead, so where the run clips, crushes or masks by value inside a cell, the colour of the cell's mean is not the mean of its pixels' colour, and the light moves by that gap. Each candidate light was handed to the GPU plan and the frame drawn with it judged against the reference frame of the view, as the release gate judges the picture at rest. A measurement of pixels, not of time: `gpu_dehaze_reduction_factor_candidates`. No factor of 16, 8, 4 and 2 draws every cell within the limits: at f = 2, 4 of 231 cells are past them, each −3 EV with Whites and Blacks at −100 under Dehaze at −100. At f = 1, where the cells are pixels, all 231 are within them.

### Scope

- **Host and build.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal; the `release` profile test binary, one run of the 100% cells and one of the Fit cells side by side, on a host shared with other agents' builds. The pixels are deterministic, so the load does not bear on them: each cell's f = 16 light drawn twice drew the same bytes on all 231 cells.
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

Dehaze's light behind Detail or an earlier Presence layer is computed exactly from that layer's output through the staged sweeps ([GPU previews](../design/gpu-preview.md#spatial-programs)); a stand-in that leaves that layer out draws a masked Dehaze behind Clarity on the Z6 past the limits (worst block 4.8 ΔE00 at 100%, 2.9 at 50%, 4.5 in export). Where the stage textures do not fit, the light is computed by a light sweep with no stage texture, which draws the layers before the light a second time ([GPU previews](../design/gpu-preview.md#spatial-programs)). A measurement of pixels; the 60 MP RAW drag stack's indication of time is under [the picture at rest and the counts after a commit](#the-picture-at-rest-and-the-counts-after-a-commit).

### Scope

- **Host and build.** The `Apple M4 Pro` adapter on Metal, macOS 26.5.2, on a host shared with other agents' builds; the pixels are deterministic, so the load does not bear on them.
- **The stage-free light.** `gpu_light_the_stage_free_light_is_the_stage_textures_bit_for_bit`: the same photographs, paths and stacks, and a masked Dehaze behind Clarity before a lens warp, each picture at rest drawn in staged sweeps and again planned with light sweeps and chained tiles, on surfaces of their own; and `a_staged_stream_reads_the_light_behind_a_spatial_layer_from_its_stage`, a 1600 × 1000 stage exported and read by a tile worker drawing it staged and by another drawing it chained after light sweeps.
- **The light.** `gpu_light_a_light_behind_a_spatial_layer_is_the_cpus_over_its_exact_prefix`: a picture at rest planned in staged sweeps at 256 px tiles and drawn by the photo surface's own drawing on a headless device, and the light its sweep kept read back, over a 1001 × 667 synthetic photograph and the 1440 × 960 Presence fixture, on the byte and linear paths, for a masked Dehaze +60 behind Clarity +60, Dehaze −100 behind a sharpening Detail and a masked Dehaze +50 behind Texture and Clarity +40 with a Basic layer between: against the CPU's preparation (`Dehaze::prepare`'s reduction and selection, in `f64`) over the stage the sweep wrote, the stack before the light drawn whole on the device and held as the stage texture holds it; against the reference render's own light; and against the CPU's light at full resolution over the exact prefix (`twin_lights` at f = 1).
- **The gate.** `cargo xtask gpu-qualification` over the Presence family, 97 stacks at Fit, 33%, 50% and 100%, the recipe `presence-dehaze-masked-behind-clarity` among them on all seven of its sources, every output kind; then the whole corpus, 46 recipes and 277 stacks (report `target/gate/whole/` in the worktree that built it).

### Results

- **The light** is the CPU's preparation over the stage the sweep wrote within 2.7 × 10⁻⁶ on every case (the existing light tolerance, 2 × 10⁻⁵, gates it), and on the linear path the reference's own light and the CPU's at full resolution within 1.0 × 10⁻⁶. On the byte path the stage texture holds half floats where the reference hands Dehaze a 16-bit frame: up to 1.3 × 10⁻⁴ from the reference's light, reported and not gated, as for a light over the source.
- **The masked Dehaze behind Clarity** is within the spatial limits at every view on every source: at rest the largest of the 28 cells is a mean of 0.019, a worst block of 0.271, a p99 of 0.652 and a signed ΔL\* of 0.002, drawn at Fit, 33% and 50% in 2 staged sweeps and at 100% by the view plan reading the light those sweeps kept; its export within the display limit on all 7 sources (largest 0.016 / 0.223 / 0.633 / 0.001), the same bytes twice; its histograms within 0.0084 code; its samples, each read through the light the tile worker's own sweeps computed, equal to the byte on screen at all 25 points of every source.
- **The Presence family**, 388 cells: every picture at rest within its limits (largest 0.058 / 0.509 / 0.807 / 0.049), and every histogram, sample and export of its 97 stacks, with no gap.
- **The whole corpus**, 46 recipes and 277 stacks, passes with no gap: all 1,108 cells of the picture at rest within their limits (pointwise largest 0.081 / 0.284 / 0.834 / 0.010, spatial 0.088 / 0.509 / 0.900 / 0.049), eight of them drawn after a light sweep, and every histogram, sample (6,925 points, each equal to the byte on screen) and export (largest 0.121 / 0.509 / 0.897 / 0.049, the same bytes twice) within its limit.
- **The stage-free light** is the staged light bit for bit on every case, and the picture at rest drawn after its light sweep the staged picture byte for byte, codes and counts; the stage-free export and its reads are the staged export's bytes on both paths, behind Clarity, Detail and before a lens warp.
- **The eight cells the stage textures did not fit**, Detail then Presence with Dehaze on the Z6 and the Air 2S at 100% and the X100VI at 50% and 100%, are drawn by light sweeps and within their limits: over those recipes on the three RAWs, 24 of 24 cells (largest 0.037 / 0.306 / 0.900 / 0.000), each light from one light sweep, the 100% cells' view plan reading it kept.
- **In motion**, not gated, the drag's frame over the reduced source at Fit, 33% and 50% is past the limits on 13 of the recipe's 21 cells there, as on many of the corpus's other spatial cells ([the picture in motion](#the-picture-in-motion)), and at 100%, where it reads the kept light, equals the picture at rest.

### Reproducing it

```sh
cargo test -p luxforge-app -- gpu_light_a_light_behind gpu_light_the_stage_free a_staged_stream_reads_the_light --nocapture
cargo run --release --locked --package xtask -- gpu-qualification --output NEW_DIR \
  --manifest /path/to/raw-manifest.json --families presence
```

## CPU and memory efficiency, measured on the M4

The [efficiency](../design/efficiency.md) work's measured results: tile slots and the rolling
window, the Detail and Presence rows, RAW row reads, the u16 demosaic input and hardware SHA-256;
and the WAL catalog, shared strokes, a brush tick's copies, collapsed sections, job reads and GPU
preview blocks.

**Host and build.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal. Everything was built
`release` with `--locked`; the `dist` profile is deferred. Files and the source cache were warm.
The one-minute load was 2.9 to 8.0 at the start of every run; the host was otherwise quiet.

**Statistics.** Every p50 and p95 is nearest-rank.

### Presence

`presence_timing` (`cargo test --release --locked -p luxforge-core --lib presence_timing -- --ignored --nocapture`).
Each stack is at +100 over a synthetic textured frame, with a warm source and warm estimates in one
render context, and 10 renders a run. In these runs Clarity alone and every stack holding Dehaze
read their reduced planes from a store after the first render; the reference renderer keeps no
such store and reduces its own estimates and computes its own reduced grids in every render, as
each run's first render here did. Texture has no reduced grid, so its rows show the kernels and
tiling alone. The cells are p50 over four runs, with each render's CPU time (p50 × CPU %).

| Stage | Stack | Render | CPU time a render |
| --- | --- | ---: | ---: |
| 6000 × 4000 | Texture | 67–69 ms | 0.85 s |
| 6000 × 4000 | Clarity | 53–56 ms | 0.70 s |
| 6000 × 4000 | Dehaze | 25–27 ms | 0.26 s |
| 6000 × 4000 | All three | 395–425 ms | 3.7 s |
| 10000 × 6000 | Texture | 172–180 ms | 2.1 s |
| 10000 × 6000 | Clarity | 154–159 ms | 1.9 s |
| 10000 × 6000 | Dehaze | 66–77 ms | 0.77 s |
| 10000 × 6000 | All three | 1253–1312 ms | 11.6 s |

- **The budget.** The spatial budget's high-water mark for all three fields at 60 MP is 208.5 MiB,
  one tile at a time.
- **All three fields.** These run one 1024 px tile at a time with pooled passes.

### Detail

`detail-performance --case render --samples 30`: Detail (luminance 25, colour 25, sharpening 40)
after Basic +0.5 EV, at full resolution with a warm context. The cells are p50 over four runs.

| Source | p50 | p95 |
| --- | ---: | ---: |
| 24 MP JPEG | 208–227 ms | 258–326 ms |
| 60 MP JPEG | 530–545 ms | 572–609 ms |

One 60 MP run whose first half ran beside other work is left out (765 ms p50).

### RAW preparation and development

**Cold open.** Measured with `cold_saved_white_balance_preparation_timing`, run with
`/usr/bin/time -l` and 15 observations a run. The clock runs from `catalog.import` to a strict
exact-source preview job, with a new owner each time:

- read, hash, decode and develop at a saved red gain of 1.1 × as-shot, then adopt;
- nothing is rendered.

Peak RSS is the whole process: its catalog setup and 15 cold opens in turn, not one open's peak.

| Source | p50 / p95 | Peak RSS |
| --- | ---: | ---: |
| Nikon Z6 (30.6 MB NEF) | 119.7–121.1 / 122.8–128.5 ms | 443–445 MiB |
| Fujifilm X100VI (85.1 MB RAF) | 245.0–250.7 / 246.7–265.7 ms | 741–742 MiB |
| DJI Air 2S (41.0 MB DNG) | 149.1–149.6 / 152.4–176.3 ms | 464–466 MiB |

Hardware SHA-256 of the original, the sized read, the writer-faulted development buffers and the
u16 demosaic input all act on a cold open; they are not measured apart. Developments of the same
sizes in turn do not accumulate retained memory in the process.

**Development.** Measured with `owner_development_timing`: 30 warm developments of the retained
mosaic at as-shot gains, after one warm-up, with `/usr/bin/time -l`.

| Source | p50 | Process peak |
| --- | ---: | ---: |
| Nikon Z6 | 36.2–36.3 ms | 458–461 MiB |
| Fujifilm X100VI | 183.3–192.8 ms | 730–731 MiB |
| DJI Air 2S | 30.7–30.8 ms | 453–455 MiB |

This loop reuses freed regions, so it cannot show what leaving zeroing to the writers saves.
Holding the sensor sites in place of a float mosaic saves 4 bytes a site in one
development's peak; the Air 2S's peak is in its DNG corrections.

The development diagnostics (`sensor_sites_match_the_float_mosaic_on_every_local_sample`) give the
bytes a development holds through its demosaic:

| Source | Sensor sites | Float mosaic | Saving |
| --- | ---: | ---: | ---: |
| Nikon Z6, Bayer RCD | 293,984,256 B | 391,976,960 B | 97,994,240 B (25%) |
| Fujifilm X100VI, X-Trans Markesteijn | 490,839,696 B | 654,446,592 B | 163,611,648 B (25%) |

**Rows through geometry.** `editor-latency --mode commit --crop 7 --samples 20` on the X100VI:
Basic exposure commits under a 16:9 crop straightened by 7°.

- **The settle** (the exact whole-stage render through the resample, plus the histogram) takes
  93–101 ms p50.
- **The committed Fit frame** takes about 19 ms.
- **The desktop's cold open** of the RAF is about 1 s, bound by platform start-up.

### The owner, catalog and desktop work

All on the 24 MP JPEG at Fit; the cells are p50 over four runs.

| Workload | p50 |
| --- | ---: |
| Basic exposure commit to the committed frame, 30 commits (the WAL catalog with full flushes) | 18.0–18.5 ms |
| The same commits to the settled histogram | 26.4–26.7 ms |
| A 400-position stroke, position to frame | 7.5–7.6 ms (p95 8.7–8.8) |
| The same stroke's `draft.set` on the owner, a tick | 0.031–0.032 ms |

- **Commits.** A durable commit on its own is not measured.
- **A brush tick's owner work.** `draft.set`'s p50 is flat along the stroke, from its first 100
  positions to its last 100.
- **The GPU stroke.** It is one frame a position.
- **Recording limit.** `editor-latency` records 400 positions unless `--samples` says otherwise
  (`PAINT_POSITIONS`).

### Outstanding

These workloads are not measured for the efficiency work; the GPU-first measurements cover launch, idle, 60 MP desktop work and masked stacks separately and do not establish the efficiency changes' individual contribution:

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

The runtime theme, the theme library and the Appearance tab ([UI themes](../design/ui-themes.md)),
under Luxforge Dark with no theme stored, as every evidence run starts.

**Host and build.** Apple M4 Pro (14 cores), macOS 26.5.2, Metal, background hidden-window
launches of `release` builds with `--locked`, warm filesystem cache. The one-minute load was 1.6
to 4.9 at the start of every run.

**Statistics.** Nearest-rank p50 / p95 in ms, every launch's samples pooled.

| Workload | p50 / p95 |
| --- | --- |
| `editor-latency --mode drag`, 24 MP at Fit, input to presented frame (GPU preview, 4 launches, 120 inputs) | 8.16 / 9.12 |
| Workspace derivation per message, `last_rederive_ms` of `slider_draft_preview` (drags drawn without the GPU preview, 248 samples) | 0.030 / 0.065 |
| `view()` of the same update | 0.066 / 0.084 |
| `measure`, empty launch to observed frame (2 runs of 5 launches) | 1778 / 1919 |

The style functions read the theme Iced passes, and a canvas cache's key holds eight bytes for it.
Split at each launch's first RSS sample above 1 MiB (samples about 55 ms apart), the launcher's
share of launch to observed frame, which includes copying the executable into the background
bundle, has a median of 690 ms, and the application's own share, from its first sample to its first
frame, a median of about 1105 ms. No theme is stored in these runs, so the launch read does not
open `themes.json`; with a theme chosen it is one bounded document read more, not measured here.
Idle CPU is 1.3 to 1.4% of one core.

## Usage statistics, measured on the M4

What optional usage statistics cost, opted out against opted in ([acceptance](../design/usage-statistics.md#acceptance-and-evidence)), against its planning budgets: a producer's p95 under 10 µs, no sustained interactive latency increase above 1 ms or 5% (whichever is larger), and no redraw attributable to collection or delivery. Every launch is eligible through explicit test eligibility with an isolated configuration, opted out (`off`) or opted in with a loopback receiver that accepts at once (`fast`), holds each request open past the transport's timeouts (`slow`) or is not listening (`offline`); none uses production reporting or anyone's consent.

**Host and build.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, background hidden-window launches of the `release` editor and a release xtask, generated 24 MP and 60 MP JPEGs, measured with `usage-measure` ([development](../engineering/development.md)). The one-minute load stayed between 2.1 and 3.6 on a 15-second log through each run, except where a row says otherwise.

**Statistics.** Nearest-rank p50 / p95. The monotonic clock ticks every 41.7 ns on Apple silicon, so a figure of 0.042 µs is one tick; the means are the samples' sums over their counts, each sample including one clock read.

| Producer, one call in the xtask process | p50 / p95 µs | Mean µs |
| --- | --- | --- |
| `record` with collection off (200,000 calls) | 0 / 0.042 | 0.014 |
| `record` into a bank with room | 0.042 / 0.042 | 0.043 |
| `record` into a full bank (dropped and counted) | 0.041 / 0.042 | 0.024 |
| `record` from four threads at once while a fifth detaches a window every millisecond | 0.71 / 1.67 | |
| `Supervisor::interacted`, each key press, click and scroll | 0.042 / 0.042 | 0.037 |
| The keyed image ID, once per open | 0.042 / 0.042 | 0.033 |
| One fresh read of the process counters the sampler takes at most once a minute (CPU time and memory; GPU allocations are left out unless the Performance section has opened the device) | 0.50 / 0.54 | |

Every producer is well inside 10 µs at p95, the contended row included.

| A catalog owner in the process, reference renderer, opted out / in | 24 MP p50 / p95 ms | 60 MP p50 / p95 ms |
| --- | --- | --- |
| `pick.develop` of one file (3 runs each) | 8.70 / 9.05 · 8.60 / 8.68 | 8.57 / 8.61 · 8.69 / 9.06 |
| `source.prepare` (3 each) | 25.9 / 27.4 · 25.9 / 25.9 | 59.2 / 62.2 · 57.4 / 57.5 |
| `edit.set-basic` commit (90 each) | 5.06 / 5.65 · 5.05 / 5.25 | 5.10 / 5.29 · 5.01 / 5.29 |
| `export.write` (9 each) | 64.2 / 85.2 · 63.8 / 84.7 | 143.9 / 160.1 · 143.7 / 159.6 |

Opted in and opted out are the same within the runs' spread: the owner's producers record committed counts and categories and nothing image-sized.

| `editor-latency --mode drag`, Basic exposure at Fit, input to presented frame (90 inputs per mode, 3 launches each, modes rotating) | 24 MP p50 / p95 ms | 60 MP p50 / p95 ms |
| --- | --- | --- |
| `off` | 8.25 / 9.13 | 8.27 / 8.50 |
| `fast` | 8.27 / 9.05 | 8.33 / 9.04 |
| `slow` | 8.26 / 9.20 | 8.24 / 9.04 |
| `offline` | 8.27 / 9.50 | 8.28 / 9.45 |

The statistics clock is moved past the first window just before each drag, so an opted-in launch sends while it runs. p50 moves by at most 0.06 ms and p95 by at most 0.95 ms (60 MP `offline`), within the 1 ms budget; a second run of the same drags (load 2.1 to 3.6) put the 60 MP `off` p95 at 9.18 ms and every mode's between 8.49 and 9.57 ms, so the p95 differences are the run-to-run spread rather than a cost of collection. Peak footprint over each launch, every mode ending with the same wait and a six-second window: 1,624 to 1,657 MiB at 24 MP and 1,896 to 1,918 MiB at 60 MP, opted out within that range (1,641 and 1,918).

The same drag over the private manifest's three RAWs on the same Mac (load 4.1 to 6.2 through these launches), each photograph opened as a RAW development:

| `editor-latency --mode drag`, RAW, input to presented frame (90 inputs per mode, 3 launches each) | Nikon Z6 p50 / p95 ms | Fujifilm X100VI p50 / p95 ms | DJI Air 2S DNG p50 / p95 ms |
| --- | --- | --- | --- |
| `off` | 8.49 / 9.11 | 8.49 / 8.67 | 8.51 / 8.77 |
| `fast` | 8.49 / 8.91 | 8.53 / 8.68 | 8.51 / 8.91 |
| `slow` | 8.49 / 8.85 | 8.49 / 8.89 | 8.51 / 9.03 |
| `offline` | 8.46 / 8.88 | 8.48 / 8.81 | 8.52 / 8.85 |

Opted in, a RAW drag's p50 moves by at most 0.05 ms and its p95 by at most 0.26 ms, within the 1 ms budget.

| Idle and hidden, one launch per mode and repeat over 24 MP, Performance closed, the statistics clock moved past the first window (6 ten-second windows per mode, load 2.1 to 3.5) | `off` | `fast` | `offline` |
| --- | --- | --- | --- |
| Presented and idle: process CPU, % of one core, p50 / p95 | 0.25 / 0.26 | 0.23 / 0.25 | 0.23 / 0.26 |
| Hidden through the native evidence step: process CPU | 0.23 / 0.27 | 0.22 / 0.28 | 0.25 / 0.28 |
| Frames drawn, views built and update-loop wakes per window, presented and hidden | 1 | 1 | 1 |

A window's own start counts one frame, one view and one update, so a statistics worker that sends, refuses or waits adds no redraw, view or wake, presented or hidden.

| Close, from the editor's shutdown event to the process's exit (3 launches each) | p50 / p95 ms |
| --- | --- |
| Quitting five seconds after the clock's move: `off` | 1226 / 1234 |
| The same, `fast` | 1222 / 1242 |
| The same, while the `slow` receiver still holds the window's request | 1233 / 1250 |
| After the drags, 24 MP and 60 MP, every mode | 1202 to 1259 at p50 |

The close is the wall clock between the shutdown event's time (the launch's start plus its elapsed time) and the exit the harness's 50 ms poll saw, so each figure is late by up to one poll. A request in flight does not delay it: quitting cancels it and nothing waits. A process that has opened the system's Metal device for GPU allocations quits about 200 ms later, so the statistics sampler leaves GPU allocations out until the Performance section has opened that device; the Performance section, expanded, costs a quit that much, which is not measured further here.

**Storage, traffic and transport, load-independent.** Measured by tests rather than by the timing tool, on the same Mac:

- The application-owned statistics heap (`luxforge-core`'s `usage_statistics_storage_stays_within_256_kib_however_long_and_hard_it_runs`, a counting allocator around the real supervisor, sampler and collector with a fake transport): 42,957 bytes before consent, a peak of 110,731 bytes in every phase (an hour of typical editing, an hour's flood, a day offline and the return), 108,732 bytes at the end of the day offline and 1,560 bytes once another process's opt-out is read: within the 256 KiB bound, with no growth while offline. Over that run, 105 requests, the largest body 65,525 bytes and the shortest gap 60 s.
- A week of mixed use on the fake clock (`usage_a_week_of_mixed_use_keeps_every_bound_with_no_daily_cap_and_no_backlog`): every gap at least 60 s, every body within 64 KiB and 512 events, nothing sent while hidden, no event older than 30 minutes, constant storage, and the seventh day's request count within three of the first's.
- A loopback capture through the real HTTP transport (`luxforge-net`'s `usage_a_loopback_capture_shows_a_request_a_minute_at_most_and_no_hourly_or_daily_cap`, JSON lines with `LUXFORGE_USAGE_TRAFFIC_CAPTURE=FILE`): 254 requests and 16.3 MB on the wire, the largest request 65,716 bytes and the shortest gap 60 s; typical editing 6 requests an hour (41 KB), a flood 59 and 60 an hour (7.8 MB in two hours), a receiver answering `503` eight attempts in a day at the cooldowns (60 s, then 20, 40, 80, 160 minutes and every 360 minutes), and 120 requests in the seven hours after it recovered, with no backlog sent.
- The transport's own heap for one 65,000-byte batch, on the sending thread (`luxforge-net`'s `usage_overhead`): plain HTTP 283 KB at its peak on the first request and 267 KB after, 16 KB left; TLS with the test roots 618 KB at its peak on the first request, which builds and keeps its client configuration (19 KB), and 600 KB after, under 1 KB left. Not counted: what the Security framework and the platform verifier allocate outside the Rust allocator, the kernel's socket buffers, and the worker's stack, a 2 MiB reservation of which only touched pages are resident.
- This Mac's device profile against `sw_vers` and `sysctl` (`usage_this_macs_profile_matches_what_the_system_reports_without_its_strings`): macOS 26.5, Apple M4 Pro tier, 14 physical and logical cores, the 64 GiB memory band for 48 GiB, within 2 KiB, and no brand string, hostname, user name or product name in it.
- Collection leaves originals, recipes, history and exports unchanged (`usage_collection_leaves_originals_recipes_and_exports_unchanged`, and its photo-sized variant over the generated 24 MP and 60 MP JPEGs with `LUXFORGE_GENERATED_FIXTURES`): the same source hashes, recipe and history (identifiers and times aside) and byte-identical exports with collection on and off.

**Outstanding.** Windows and Linux (no native measurement), a TLS receiver in a timing run (the timed receivers are plain HTTP on loopback; TLS cost is the test's heap figure above) and an import timed in the editor rather than in the owner. A `process.sample` needs a minute of real presented activity, which the evidence clock's moves do not give, so no launch here recorded one.

## Software adapters

A session with no usable hardware adapter first tries the platform's software adapter (lavapipe on Linux, WARP on Windows) and draws through the GPU path on it, once it is measured fast enough that dragging is not badly laggy ([decisions](../decisions.md#gpu-first-rendering), [GPU-first](../design/gpu-first.md#proposals-with-recorded-defaults)). This is that measurement, against thresholds that await the owner's confirmation: at 24 MP at Fit, a drag's tick at p95 within 33 ms for a pointwise stack and within 100 ms for a Detail and Presence stack. **Lavapipe misses them**, so it is not adopted (`adapters::SOFTWARE_ADAPTER_ADOPTED` stays off): a pointwise drag's Fit tick is 2.7 to 7.3 times the 33 ms and a Detail drag under Presence 3.5 to 7.2 times the 100 ms.

### Scope

- **Not a typical PC.** An arm64 Linux container on the owner's M4 Pro, through Docker Desktop 29.4.0, whose Linux VM (kernel 6.12.76-linuxkit) has 14 vCPUs and 7.9 GiB; no GPU is passed through. The image is `rust:1.94.0-trixie` with Debian trixie's `mesa-vulkan-drivers` 25.0.7: lavapipe, `llvmpipe (LLVM 19.1.7, 128 bits)`, Vulkan 1.4.305, a `Cpu` adapter, chosen by `WGPU_BACKEND=vulkan` and `VK_ICD_FILENAMES` naming `lvp_icd.json`. The M4's cores are faster than most PCs', so these figures flatter lavapipe; macOS schedules the VM's vCPUs on its performance and efficiency cores as it chooses. WARP and Windows are not measured.
- **Two CPU counts.** Docker's default, every vCPU (`nproc` 14), and `--cpuset-cpus 0-3` (`nproc` 4), so lavapipe's threads and the reference renderer's pool are four, approximating an ordinary four-core machine.
- **The baseline.** The M4's own Metal adapter, natively, through the same benchmark.
- **Build.** Release, `--locked`; the container's benchmark built in the container.
- **Load.** Other sessions share the host. The figures below are each configuration's quieter run: one-minute load 4.5 to 4.8 for Metal, 5.4 to 9.2 for 14 vCPUs and about 5 to 8 for four. A first run of each at loads up to 33 is given beside them where it differs: the two four-vCPU runs agreed within 8%, the two 14-vCPU runs did not, so the loaded 14-vCPU figures are an upper bound.

### What is timed

The drag-tick benchmark (`crates/luxforge-app/src/app/gpu_drag_bench.rs`), an ignored release test, on the generated 24 MP JPEG (6000 × 4000):

- **A tick.** A draft of the stack's dragged slider, one value a tick swept to and fro, planned by the catalog owner as the desktop plans a gesture's tick (outside the timing; 0.01 to 1.3 ms), then drawn through one slot of the photo surface's own drawing on a headless device with the change measured since the tick before, timed from handing the source to the queue's completion, so the GPU's work is counted, not its encoding. 40 timed ticks a view after one untimed tick that waits out the compile and the source's upload; the last tick's frame equals the same plan drawn fresh in every run. **Fit** is the evidence window's bounds, a 1716 × 1144 frame; **100%** is that window's visible region at full scale, 1798 × 1662.
- **The picture at rest at Fit.** The stack's tiles at full resolution reduced to the view, one tile a frame as the surface draws them, after an untimed draw that compiles; a draw past 20 s is measured once, its compile included.
- **Export.** The stack's output stage streamed through the desktop's GPU tile worker, once cold and once warm, the warm figure given (the cold one where it passed 20 s).
- **The reference renderer's whole frame per tick.** The drafted stack's exact 24 MP frame, the frame a session without a GPU draws per tick, 8 frames a stack.

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
Whether a GPU-less host should draw a reduced-size CPU frame per tick rather than the whole reference frame is the owner's question; these figures do not answer it.

### Reproducing it

Natively, after `cargo xtask generate-fixtures`:

```sh
LUXFORGE_DRAG_BENCH_OUTPUT=/absolute/NEW_DIR cargo test --release --locked -p luxforge-app \
  --bin luxforge app::gpu_drag_bench::drag_tick_benchmark -- --ignored --exact --nocapture
```

It writes `drag-ticks.json` (every tick, the adapter, the host and the environment) and `drag-ticks.md`; `LUXFORGE_DRAG_BENCH_TICKS`, `LUXFORGE_DRAG_BENCH_REFERENCE_TICKS` and `LUXFORGE_DRAG_BENCH_STACKS` (`basic`, `basic-curve-mixer`, `detail-presence`, `detail-presence-detail-drag`, `masked-presence-3`) measure less, and `LUXFORGE_GPU_ADAPTER=software` asks wgpu for the software adapter alone on a host that has a hardware one too.

In the container: an image `FROM rust:1.94.0-trixie` adding `build-essential pkg-config cmake nasm clang libx11-dev libxkbcommon-dev libwayland-dev libvulkan-dev libvulkan1 mesa-vulkan-drivers vulkan-tools jq` (and, for the editor's own launches under Xvfb, `xvfb xauth libxkbcommon-x11-0 libxcursor1 libxi6 libxinerama1 libxrandr2`, without which it panics before its window opens), with `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`, `WGPU_BACKEND=vulkan`, `CARGO_TARGET_DIR=/target` and `CARGO_HOME=/cargo`; the worktree mounted read-only at `/src`, named volumes at `/target` and `/cargo` so the host's `target/` is untouched, and a writable directory for the output. Build with `cargo test --release --locked -p luxforge-app --bin luxforge --no-run`, copy the test executable out, and run it with `app::gpu_drag_bench::drag_tick_benchmark --ignored --exact --nocapture` and `LUXFORGE_DRAG_BENCH_OUTPUT`, once as it is and once with `docker run --cpuset-cpus 0-3`.

## Corrections qualified on the M4

The offline Clone/Heal workflow's native qualification ([Corrections](../design/corrections.md)): the release gate over the repair families, the RAW journeys and the photographic review. Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, the `Apple M4 Pro` adapter on Metal, on a host shared with other agents' builds, whose load bears on no figure here: every one is a pixel or a pass, not a time.

### The repair families against the reference

`verify --tier rendered --manifest` with the private three-source manifest ran the whole corpus through the gate: 341 stacks, every picture at rest within its class at every view, every sample, and every histogram and export but one stack's. The repair families are 36 stacks, 144 views at rest: Clone and Heal, spots and strokes, a chain whose sources are earlier destinations, an overlap before Detail, a stroke healed from across the frame, a spot at the frame's edge, and the same corrections under a straightened crop and a Perspective warp (the four generated JPEGs) and under the source's lens profile and a warp (the lens zone plate and the three RAWs). Mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / \|signed mean ΔL\*\|, the largest over the family's views at rest, against the spatial limits 1.0 / 2.5 / 5.0 / 0.5:

| Family | Sources | Stacks | At rest within | Largest at rest | Histogram, sample, export within | Motion past a limit (not gated) | Largest in motion |
| --- | --- | ---: | ---: | --- | --- | ---: | --- |
| Clone | 4 JPEG | 8 | 32 of 32 | 0.000 / 0.026 / 0.00 / 0.000 | 8, 8, 8 of 8 | 4 of 32 | 0.04 / 18.8 / 0.0 / 0.00 |
| Clone | 3 RAW | 6 | 24 of 24 | 0.017 / 0.206 / 0.59 / 0.000 | 6, 6, 6 of 6 | 12 of 24 | 1.47 / 6.19 / 6.05 / 0.17 |
| Heal | 4 JPEG | 8 | 32 of 32 | 0.011 / 1.135 / 0.38 / 0.011 | 7, 8, 7 of 8; 1 gap each | 6 of 32 | 2.80 / 12.7 / 10.2 / 3.48 |
| Heal | 3 RAW | 6 | 24 of 24 | 0.024 / 0.244 / 0.68 / 0.000 | 6, 6, 6 of 6 | 9 of 24 | 1.47 / 6.19 / 5.98 / 0.20 |
| Heal under geometry | 4 JPEG, lens zone plate | 5 | 20 of 20 | 0.005 / 0.027 / 0.29 / 0.000 | 5, 5, 5 of 5 | 12 of 20 | 12.5 / 28.1 / 40.7 / 2.21 |
| Heal under geometry | 3 RAW | 3 | 12 of 12 | 0.017 / 0.212 / 0.60 / 0.000 | 3, 3, 3 of 3 | 6 of 12 | 1.62 / 5.62 / 6.61 / 0.19 |

- **The gap.** `repair-heal-spots` on the 60 MP JPEG: in the gate run, its stroke healed from across the frame needed a window wider than the device's 8,192-texel limit, so the tile worker could not draw its tiles (`unplannable`) and the export was the reference's (`texture-limit`). Its picture at rest and samples are within the limits; its histogram and export are unmeasured on the GPU, and the gate is incomplete for that stack alone. A repair step holds its read set in an atlas of its own, so a tile's window is its own rectangle and halos ([contract](../design/corrections.md#rendering)); the tile tests draw a Heal and a Clone sampling across a 9,000-pixel frame within the spatial class, but the gate's rerun over this stack is set aside, so this stack is not qualified at the gate.
- **Motion.** A drag's frame at Fit, 33% and 50% evaluates the repair at the reduced stage, where offsets round to that stage's pixels and Heal solves on its pixels ([numerics](../design/corrections-numerics.md#reduced-stages)); the zone plate and the lens zone plate, whose chirp aliases at any reduction, carry the largest differences. Reported, not gated, as for every family.
- **Exact references.** The CPU units are held bit for bit to the independent `f64` reference and the repair frame to it on JPEG bytes and RAW-precision values by the ordinary tests (`repair::`, `render::repair_tests`), which `check` ran in the same tier.

### The RAW journeys

`smoke --scenario raw-corrections` over each manifest source (`artifacts/qualify/raw-corrections-*-2` in the qualifying worktree), six background launches each, 64 checks, all passing on the Z6 NEF, the X100VI RAF and the Air 2S DNG: a Clone's destination reads its source's developed pixels within 12 codes; history preview, Restore and its undo, reopen and Locate show the photograph as they should; the overlay is placed by the core's `render.transform` through the lens profile (applied on the Z6 and Air 2S; the X100VI has none) and a Perspective warp; and each original keeps its hash. The GPU's exports against the reference export of the same entry, over every pixel, by the spatial limits:

| Source | Corrections only | Under the lens profile and a warp |
| --- | --- | --- |
| Air 2S DNG, 5464 × 3640 | 0.196 / 2.26 / 2.44 / 0.000 | 0.200 / 2.23 / 2.37 / 0.000 |
| Z6 NEF, 4024 × 6048 | 0.067 / 1.57 / 1.58 / 0.000 | 0.043 / 0.99 / 1.30 / 0.000 |
| X100VI RAF, 7728 × 5152 | 0.001 / 0.39 / 0.00 / 0.000 | 0.004 / 0.51 / 0.00 / 0.000 |

The run's own owner, with no GPU, exported the warped entry through `export.plan`, `batch.export` and `export.write`, each read with `job.read`: the batch and single files are the same bytes, their pixels are the desktop's reference export's exactly, and the desktop's GPU export is within the same limits of them. The Air 2S's worst block, 2.26 of 2.5, is the closest any export comes to a limit.

### Timing

Interaction latency, render, sample and export times, memory and idle cost with a correction stack are not measured. The timing, memory and idle runs are set aside, along with the `gpu-qualification` rerun of the 60 MP far-source Heal stack and the gate run of the `repair-patch` family; none has run, and a skipped measurement is not a pass. `verify --tier full --manifest` would run `editor-latency --mode corrections` on the 24 and 60 MP JPEGs and each manifest RAW.

## HDR merges, measured on the M4

What an HDR merge costs at photo size ([acceptance](../design/hdr-merge.md#evidence-and-acceptance)): synthetic handheld brackets of a moving ball (`luxforge_testkit::hdr_bracket` at 6000 × 4000 and 9504 × 6336, 16-bit Bayer DNGs of a RAW test camera, ±2 EV steps for three frames and 1 EV steps for nine), merged through the JSON API by an in-process owner over a new catalog, the reference renderer and no GPU client in the process. The merge itself is CPU work; the GPU is not involved.

**Host and build.** Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, internal SSD at 92% full, a release xtask and the release editor for the drag, measured with `hdr-merge-measure` ([development](../engineering/development.md)) behind the host timing lock, with other agents' work on the host. 24 MP × 3 and 60 MP × 9 were timed at one-minute loads of 2.5 to 9.7, briefly 12.3 during the 60 MP × 9 editor work, and 60 MP × 3 at 5.3 to 10.9. 24 MP × 9 was timed only at 12.5 to 45, so its times are unreliable and stand only as an upper bound; so are the reopen times of 24 MP × 3 and 60 MP × 9, taken at 18 to 35. Memory, disk and object figures do not depend on load and were the same in every run. Cold is a worker process's first merge, its own owner and catalog new; warm is a later merge in the same process, each into a new catalog. Neither purges the operating system's file cache, which the brackets were just written into.

**Statistics.** Nearest-rank p50 / p95, 3 cold and 5 warm merges per workload; stages from the job's phases on the activity board, so reading runs from the request.

| Merge stage, cold · warm, p50 / p95 ms | 24 MP × 3 | 60 MP × 3 (load 5.3 to 10.9) | 24 MP × 9 (load 12.5 to 45) | 60 MP × 9 |
| --- | --- | --- | --- | --- |
| Total | 2207 / 2213 · 2175 / 2206 | 5674 / 6381 · 5658 / 5802 | 9295 / 14026 · 10753 / 15856 | 53482 / 64345 · 52085 / 58488 |
| Reading: each input read, hashed, developed and spooled | 843 / 884 · 817 / 827 | 2248 / 2501 · 2149 / 2263 | 3909 / 4408 · 3180 / 3755 | 6472 / 6882 · 6883 / 7554 |
| One input of those | 283 / 308 · 271 / 279 | 743 / 859 · 716 / 761 | 418 / 543 · 345 / 453 | 725 / 787 · 762 / 843 |
| Aligning: proxies, registration, motion | 162 / 164 · 158 / 160 | 155 / 177 · 150 / 159 | 847 / 1354 · 885 / 1283 | 603 / 615 · 617 / 683 |
| Merging: fusion, chunk by chunk | 1151 / 1168 · 1158 / 1193 | 3199 / 3655 · 3296 / 3498 | 4710 / 8029 · 5522 / 12043 | 46298 / 56676 · 44244 / 50054 |
| Saving | 34 / 34 · 36 / 38 | 49 / 70 · 46 / 61 | 159 / 235 · 182 / 193 | 170 / 173 · 293 / 341 |

- **60 MP × 9 fusion is bound by its spool reads.** These figures are of fusion reading each input's window of a chunk back from its spool one row at a time, about 20 KB a read at this width; fusion reads in bands of up to 8 MiB, which is not yet measured. One more cold merge, sampled every 3 s: through its 61 s of merging, the worker used 16 to 47% of one core while the disk served 7,200 to 8,500 reads a second at 134 to 215 MB/s. The 7.1 GiB of spool is not reliably still in the file cache when fusion reads it. Across this run and a busier one (one-minute load 15 to 33), the 16 merges' merging took either 9.9 to 24.0 s (7 merges, one of them in the quiet run) or 40.6 to 56.7 s (9 merges), whatever the load. That fits the spool being cached or not. So a 60 MP × 9 merge takes between about 17 s and 64 s on this Mac. The three-frame and 24 MP workloads are not seen to vary this way.

| Memory and disk, cold and warm alike, in every run | 24 MP × 3 | 24 MP × 9 | 60 MP × 3 | 60 MP × 9 |
| --- | --- | --- | --- | --- |
| Accounted scratch at its peak, against the 256 MiB limit | 104.4 MiB | 188.4 MiB | 103.9 MiB | 187.9 MiB |
| The one development held outside it | 276.5 MiB | 276.5 MiB | 692.0 MiB | 692.0 MiB |
| Temporary spool at its peak, against the 16 GiB ceiling (14 bytes a pixel an input) | 961 MiB | 2,884 MiB | 2,412 MiB | 7,236 MiB |
| Spool files left after success or cancellation (cold, warm and cancelled merges) | 0 | 0 | 0 | 0 |
| The saved result under the catalog's artifacts: chunks and manifest | 270.2 MiB | 269.2 MiB | 678.0 MiB | 675.4 MiB |
| Process footprint at its largest while merging, cold p50 / p95 (sampled every 100 ms) | 575 / 578 MiB | 739 / 743 MiB | 1,195 / 1,199 MiB | 1,350 / 1,358 MiB |
| Peak footprint of the warm worker over its whole life (every merge, cancellation and the editor work beside them; this depends on how much editor work ran, 1,421 MiB at 60 MP × 9 in the busier run) | 760 MiB | 839 MiB | 1,345 MiB | 1,595 MiB |

The footprint is the process's physical footprint from `resources.read`, which also holds the owner, the catalog and the allocator's retained pages. It grows with frame size, and from three frames to nine only by the scratch and 70 to 80 MiB more: no full frame is retained per input. The largest footprint is 194 to 470 MiB more than the scratch peak and the development together. These measurements do not attribute that difference, and they make no total-memory bound claim.

| Cancellation: `job.cancel` to the job's end, p50 / max ms (3 each; every one cancelled, nothing published) | 24 MP × 3 | 60 MP × 3 | 60 MP × 9 |
| --- | --- | --- | --- |
| While reading the second input | 2.4 / 3.0 | 5.9 / 7.5 | 4.5 / 8.6 |
| As alignment begins | 8.9 / 12.0 | 42.5 / 47.1 | 155 / 178 |
| Half way through the chunks | 20.6 / 23.0 | 56.6 / 60.2 | 233 / 249 |
| As the last chunk is written | 20.0 / 21.3 | 38.2 / 52.2 | 383 / 423 |

At 24 MP × 9 (load 12.5 to 45) cancellation took at most 5, 120, 133 and 63 ms.

| Reopen and redo, each workload | 24 MP | 60 MP |
| --- | --- | --- |
| HDR merge activity through undo, redo, reopening, preparation and export, with the inputs moved away | 0 | 0 |
| `library.undo` / `library.redo` of the merge's creation, after reopening, p50 / max ms | 4.8 to 5.3 / 9.0 · 4.9 to 5.0 / 11.1 | 5.0 to 8.9 / 9.8 · 5.0 to 5.2 / 13.4 |
| `source.prepare` of the result: its chunks read and verified into one frame | 215 to 220 ms | 518 to 737 ms |
| The footprint that preparation adds: the frame | 282 MiB | 688 to 691 MiB |
| `export.write` of the result, reference renderer | 101 to 110 ms | 206 to 210 ms |

Each reopen merged copies of the bracket and undid and redid the merge in its own process. It then moved the copies away and reopened the catalog in a new process: `merge.inspect` reported every input missing and the source available. Through undo, redo, preparation and export no HDR merge began, progressed or ended on the activity board, and the objects under `sources/` and the merges' working folders were unchanged. With the inputs gone, nothing could read one.

| Editor work through the same owner, reference renderer, p50 / p95 ms: idle · beside a merge | 24 MP × 3 | 60 MP × 9 |
| --- | --- | --- |
| `browse.rows` of 200 over a view of 1,000 JPEGs | 0.7 / 0.7 · 0.6 / 0.7 | 0.6 / 0.8 · 0.7 / 2.2 (max 9.4) |
| `render.sample` of a prepared 24 MP JPEG | 0.0 / 0.5 · 0.0 / 0.1 | 0.1 / 0.5 · 0.1 / 0.7 (max 11.1) |
| `edit.set-basic` commit | 4.8 / 8.1 · 4.9 / 7.0 | 5.5 / 10.7 · 5.5 / 10.2 (max 92.0) |
| `export.write` of it | 103.6 / 105.2 · 96.3 / 119.6 | 110.2 / 110.2 · 110.1 / 218.0 (max 308) |
| The merge they ran beside, against warm alone (p50) | 2,316 against 2,175 | 74,126 against 52,085 |

There were 10 idle samples of each operation. Beside a merge there were 23 of each at 24 MP and about 500 at 60 MP (load up to 12.3 at its end), counting only those that ended while the merge still ran, so a 60 MP export ran beside it back to back throughout. At 24 MP × 9 and 60 MP × 3, browse, sample and commit beside a merge were no slower than idle at p95. Editing beside a merge keeps its own times; the merge takes longer.

| `editor-latency --mode drag`, Basic exposure at Fit on the 24 MP JPEG, input to presented frame, 30 inputs, p50 / p95 / max ms | Quiet run (load 7.0 to 9.7) |
| --- | --- |
| Alone | 8.4 / 8.7 / 9.4 |
| While another process merged 60 MP × 9 | 8.9 / 25.3 / 56.2 |

The drag fell within that merge's reading stage, the decoding and development of its inputs, its most CPU-intensive. A drag during fusion, and one in the same process as the merge, are not measured.

**Natural brackets.** The private manifest's sources are three single RAWs, from a Z6, an X100VI and an Air 2S. Indexed read only into a new catalog, they formed no bracket in Select. Natural-bracket quality (motion, seams, colour, highlights, shadows and crop losses on real brackets) is unqualified.

**Outstanding.** GPU allocations while the editor shows a merged result are not measured, because the worker has no GPU client and `resources.read` reports GPU memory unavailable. Also outstanding are 24 MP × 9 times on a quiet host, a drag during fusion, fusion's banded spool reads, and Windows and Linux.

## High-zoom minimap

Native Apple M4 Pro / Metal, optimized release, background evidence launches with a warm filesystem cache, generated 24 MP (6000 × 4000) and 60 MP (10000 × 6000) JPEGs at a 2× display scale. One-minute load 2.5 to 3.7 at each run's start, below the 8.0 threshold. `editor-latency --mode minimap` drags the card's viewport rectangle through 40 moves at the stated zoom and times each move from its pan to the first draw that shows it: on the GPU, the picture at rest planned for that view (`plan`); on the reference renderer (`--no-gpu-render`), the retained frame scrolled into place (`scroll`). 200% and 800% have different view windows, so the rows are not an isolated rendering comparison; the card is not shown below 200%, so a 199.9% control is not part of this mode and is not taken.

| Source | Zoom | Renderer | Drawn | Pan to frame ms p50 / p95 | First overview ms, cold / warm | Overview GPU bytes, retained / peak |
| --- | ---: | --- | --- | ---: | ---: | ---: |
| 24mp | 200% | gpu | plan | 17.2 / 17.4 | 65.0 / 77.0 | 175,200 / 1,289,376 |
| 24mp | 800% | gpu | plan | 17.2 / 17.7 | 67.3 / 75.8 | 175,200 / 1,289,376 |
| 24mp | 200% | reference | scroll | 8.3 / 8.3 | 62.7 / 55.4 | 175,200 / 175,200 |
| 60mp | 200% | gpu | plan | 17.3 / 17.4 | 143.8 / 210.7 | 157,792 / 1,337,504 |
| 60mp | 800% | gpu | plan | 17.3 / 17.6 | 147.1 / 209.8 | 157,792 / 1,337,504 |
| 60mp | 200% | reference | scroll | 8.3 / 8.3 | 80.7 / 70.9 | 157,792 / 157,792 |

Cold is the first overview after crossing 200%; warm the one made again after Select and Develop. In every run the 40 moves reuse the overview with no build, sweep or upload: the GPU runs make 2 GPU builds and 2 sweeps in all, the reference runs 2 reference builds and 2 uploads, both from the two first overviews; no CPU bytes are retained. The overview's peak, the accumulator and tables while one is built, is within the 4 MiB cap. Process memory and GPU allocation are recorded per run in the evidence (`artifacts/qual-sweep/timing-3/minimap-*`); they are the editor's at that zoom, not the minimap's, and make no total-memory claim. Idle behaviour is not measured separately.

**Declared bounds** ([architecture](../design/architecture.md#limits)), each held by tests rather than measured:

| What | Bound |
| --- | --- |
| The overview raster | Aspect-fitted within 256 × 256 physical pixels (`OVERVIEW_BOUNDS`) |
| Its GPU storage per pipeline: accumulator, coverage tables and parameters, finished textures and their uniforms, current, replacement and retiring | 4 MiB together (`OVERVIEW_GPU_BUDGET`, `OVERVIEW_GPU_BYTES`), charged inside the 2 GiB GPU-preview budget before creation; a picture at rest planned at 100% and above leaves it free in its share |
| Overviews a photo surface keeps | 2, the settled one and one replacement |
| The reference's reduction: output, coverage tables and one band of rows | 1 MiB (`OVERVIEW_CPU_BYTES`), one at a time on the blocking pool, the retained frame borrowed |

**Counted, not timed.** The background `zoom` journey on generated 24 MP and 60 MP JPEGs on the M4 Pro's Metal adapter (`artifacts/minimap-integration` in the integration worktree) records the overview's lifecycle counters in every frame's state: from the first ready overview through the 1600% pans, the 199.9% control and back to 200%, and every minimap click, drag, API pan, wheel and letterbox press, the GPU and reference builds, sweeps and uploads stay unchanged while the reuse count rises (24 MP: 1 GPU build and 1 sweep, reuses 1 to 11; 60 MP: 1 and 1, reuses 1 to 10); the crop draft's stage and Compare's After each add one reference build and one upload. The 6000 × 4000 stage's overview is 256 × 171. The journey's checks assert placement, rectangle, pan and pixels, not these counters, which are read from its record and are not a cost measurement.

**Outstanding.** A 199.9% control, idle behaviour with the card shown, oversubscribed memory in a real window, and Windows and Linux.

## Range picks, measured on the M4

Native Apple M4 Pro / Metal, optimized release, background evidence launches with a warm filesystem cache, generated 24 MP and 60 MP JPEGs at Fit, with the range experiment on, Mask mode entered and one unbound colour range created from New mask. `editor-latency --mode pick --samples 30` alternates a whole-frame drag pick (the output stage's top-left pixel to its bottom-right one) and a click pick on the red quadrant, each timed from the press to the pick's command sent and to the first coverage overlay of the entry it committed. `--reference-renderer` launches with `--no-gpu-render`. The renderer is the one `mask.sample-input-area` names for the drag.

| Source | Launch | Area read by | Drag ms p50 / p95 | Click ms p50 / p95 | Load at start |
| --- | --- | --- | ---: | ---: | ---: |
| 24mp | gpu | gpu | 222.3 / 245.6 | 18.2 / 27.7 | 10.79 (unreliable) |
| 24mp | reference | reference | 266.9 / 273.7 | 18.1 / 18.4 | 8.86 (unreliable) |
| 60mp | gpu | reference (`texture-limit`) | 642.2 / 647.2 | 25.4 / 29.5 | 6.92 |
| 60mp | reference | reference | 644.6 / 660.5 | 18.3 / 34.5 | 5.17 |

The 24 MP rows ran above the 8.0 load threshold in each of three runs; the three agree within 3 ms at p50 (GPU drag 222 to 225 ms), but they are not a baseline. Pressing to the command sent is at most 0.5 ms in every row: the time is the read and the commit. At 60 MP the whole frame's area plan is wider than the device's 8,192-pixel texture limit, so the GPU launch's drag is read by the reference, as its reason says; the GPU area read has no tiled path. A click reads a 1 × 1 area on either renderer. Evidence: `artifacts/qual-sweep/timing-3/pick-*` (and `timing`, `timing-2` for the other two runs).

**Outstanding.** A quiet-host 24 MP run; a tiled GPU area read for stages past the texture limit; Windows and Linux.

## Method

Optimized builds only, with commit, lockfile, OS, CPU/GPU, RAM, display and storage recorded. Report cold and warm runs separately and say which cold is meant. Keep at least 30 samples and never drop failures or tails silently. Measure user event to presented frame, not shader time, and account CPU RSS, cache bytes, GPU allocations and transient copies without double-counting unified memory. Capture idle after all background work stops. No timing gates in CI; CI enforces exactness, deterministic bounds and coverage. VM checks record hypervisor, guest graphics path and software versus accelerated rendering, and never stand in for native timings.

Correctness checks use deterministic fixtures across all EXIF orientations, gradients, patches and fine detail: coordinate round trips, crop coverage, CPU versus GPU differences and export interpretation, including centered Option resizing and angle sweeps that return without cumulative shrinkage. Color and export proofs choose explicit numerical tolerances and viewing conditions.

## Module activation

Logical boundaries and user enablement do not by themselves reduce memory or launch time. After the first external use case is selected, compare the built-in baseline with an activation prototype on the M4: minimal and default configurations, disabled and enabled-but-unused modules, first use, cold and warm launch, RSS, CPU/GPU allocations, idle work and first-use latency, shader compilation and warm-up on both GPU devices, with packaging size reported separately. Processing-module outputs must retain exact CPU reference fixtures and pass required corpus qualification within the declared GPU tolerance. Disabled modules must start no workers and allocate no processor resources. Reject any optimization that changes output or skips a recipe effect. If the benefit is immaterial, keep the simpler built-in implementation; the external-loader requirement remains regardless.

## Growth rules

Catalog opening never enumerates or decodes all originals. Grid memory depends on visible items and cache quotas. Import, hashing, thumbnailing and indexing use backpressure and resumable batches. Switching photos cancels obsolete preview requests, and the app stays interactive during exports and indexing. Introduce tiling with neighborhood halos before promising unrestricted large-image local processing. A cleanly reported resource limit is acceptable; silent exhaustion is not.

## Catalog-edit comparison

Native Apple M4 Pro / Metal, release: three runs over the owner's DSC_3187.NEF
(4024 × 6048), using isolated copies of the current catalog's thirteen-layer, three-mask edit. A
global Exposure change from 0.32 to 0.33 EV recreates the freshly committed GPU presentation; the
original edit and source remain untouched. The hidden window is 1440 × 900 points at scale 2.
Entry is measured from the scripted backslash tap to the verified renderer readback with both
comparison surfaces ready, including capture overhead; this is not scanout latency or cold-open
performance. Each run opens and warms the photograph first. The host-wide timing gate is held and
starting one-minute load is 2.84–4.86, below the threshold of 8.

| Path | Three entry-to-readback observations (ms) | Median (ms) |
| --- | --- | --- |
| Retained GPU After plans and source | 728.3, 750.8, 752.2 | 750.8 |

The comparison's second GPU surface evaluates the retained picture, and the edited picture stays
visible during that handoff. In all three runs the After endpoint and restored picture match the
warmed edited picture exactly in the same 1050 × 1420-pixel photographic rectangle, excluding
divider and label chrome. Captured state records the fixed After picture, the Original Before and
the GPU drawing paths at Fit, through 100%, and on exit. A separate native white-balance check
retains the exact After fallback when Before needs another RAW development. The executor holds one
prepared source; reusing GPU-only After across that change stalls source residency, so the retained
After is limited to shared-source comparisons.

A comparison entered at Fit retains that display detail when magnified; entering at 100% or
above, with changed RAW sensor gains or historical selection, with clipping marks, without eligible
whole GPU content, or near the shared GPU budget keeps the exact reference After fallback. No general editor memory or latency qualification is
implied. Local evidence is `artifacts/editor-repairs-comparison/verified.json`, with command lines,
binary hashes, source hash, load, captures, state and events beside it; RAW source checks are in
`artifacts/editor-repairs-raw-source/validated-app`, and mask/navigation checks in
`artifacts/editor-repairs-masks` and `artifacts/editor-repairs-navigation/selection-checked-app`.
The comparison eligibility and source-restoration checks are in
`artifacts/editor-repairs-comparison/current-build/verified.json` and
`artifacts/editor-repairs-raw-source/verified.json`; their native runs qualify behavior, not timing.
