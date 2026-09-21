# Performance measurement plan

Status: provisional budgets, not accepted requirements. The owner's M4 MacBook Pro is the reference machine. The engineering rules every core and desktop change must follow are in [performance rules](../engineering/performance-rules.md). Fast interaction, background throughput and output correctness are evaluated separately.

## Reference workloads

| Workload | What it reveals |
| --- | --- |
| 24 MP and 60 MP JPEGs, EXIF-rotated variants, embedded sRGB/Adobe RGB/Display P3 profiles | First-open latency, memory, geometry and color |
| Huge or invalid dimensions, truncated files, malformed profiles | Resource bounds and error recovery |
| M4 with its actual display scale recorded; optional external SDR 4K | Preview, input, color, DPI and Metal resource measurements |
| Linux ARM64 VM, later native Windows/Linux GPU machines | Functional portability versus native GPU behavior, measured separately |
| 100,000 metadata rows; 1,000,000-row stress catalog | Index selection, pagination and startup independent of image bytes (later library) |
| At least 1,000 real images, then a larger owner dataset | Thumbnail decode and cache behavior synthetic rows cannot show |
| Local SSD, later removable SSD and NAS | CPU/GPU throughput versus storage latency |
| Nikon Z6 NEF and Fujifilm X100VI RAF in the owner's real modes | RAW decode/development, WB redevelopment, history, presentation and peak memory; see the [RAW integration contract](../design/raw-integration.md) |

Datasets need provenance, dimensions, profile and orientation, and redistribution permission. Synthetic fixtures live in the repository; private originals stay in a local manifest and are never committed.

## Provisional budgets

Engineering hypotheses until measured and accepted on the recorded M4 configuration, in SDR with the display contract recorded.

| Metric | Proposed budget |
| --- | --- |
| Launch to usable empty shell | p95 < 1 s warm, < 2 s cold |
| Uncached 24 MP JPEG to Fit preview | p95 < 750 ms; loading feedback within 100 ms |
| Crop overlay frame time | p95 ≤ 16.7 ms at 60 Hz |
| Geometry input to presented preview | p95 < 50 ms once the source preview is ready |
| Empty steady-state process memory | ≤ 150 MiB including helper processes |
| 24 MP single-image edit working set | ≤ 600 MiB CPU-resident |
| 60 MP import or export peak | ≤ 1 GiB process RSS, GPU memory reported separately |
| Idle CPU | < 1% of one core over 30 s after background work settles |
| First page of a 100,000-row indexed filter | p95 < 100 ms warm (later library) |
| Warm adjacent-image Fit preview | p95 < 150 ms on a cache hit (later library) |

A single float32 RGBA buffer for 60 MP is about 916 MiB, so unrestricted full-resolution float processing needs tiling before it is promised.

## Recorded baselines

Native M4 Pro, release builds, warm filesystem cache, synthetic fixtures. Diagnostic observations, not accepted budgets or cross-platform claims.

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

With the crop module, `editor-performance` on 24 and 60 MP, 30 samples each, warm cache. The crop
rows render the whole stack: 200 composed exact transforms and then one straightened crop, whose
resample is a stage boundary, so the difference between the two rows is the interpolating pass.

| Measurement | 24 MP | 60 MP |
| --- | --- | --- |
| Core render of one exact transform (p50 / p95) | 10.7 / 11.2 ms | 22.7 / 26.0 ms |
| Core render of 200 composed exact transforms (p50 / p95) | 10.7 / 11.1 ms | 22.5 / 24.2 ms |
| The same stack with a 10° `crop-fit` on top (p50 / p95) | 33.2 / 37.7 ms | 70.5 / 77.0 ms |
| Crop output stage that measures | 3695 × 2077 from a 4000 × 6000 input | 5542 × 3116 from a 6000 × 10000 input |

With the orientation layer and content-space placement, the 24 MP rows re-measured on the M4 Pro (release, 30 samples, warm cache, core request-to-render only) at 10.3 / 14.3 ms for one transform, 10.5 / 11.1 ms for 200 transform actions folded into one orientation layer and 31.5 / 34.0 ms with the 10° `crop-fit` on top; the crop-fit commit fell from 1.2 to 0.6 ms because it plans against a one-layer prefix instead of two hundred. 60 MP was not re-measured.
| `crop-fit` commit: validation, fitting, compile and persistence, no render | 1.3 ms | 0.9 ms |
| Identity render from the cached decode (shared buffer, no copy) | under 0.01 ms | under 0.01 ms |

Editor process measurements from `measure`, five app-cold launches per workload plus one repeated
60 MP run, on the same host. Launch to observed frame is an upper bound: it includes the harness's
capture readback, not scanout.

| Measurement | Result |
| --- | --- |
| Launch to observed frame (empty / 24 MP / 60 MP) | median 231 / 285 / 396 ms |
| Open request to captured frame (24 / 60 MP) | median 196 / 303 ms, of which upload 27 / 67 ms |
| Sampled peak RSS (empty / 24 / 60 MP) | 124 / 469 / 987 MiB; 1151 MiB after sixteen 60 MP loads |
| Idle CPU with a 60 MP image open, 30 s after settling | 1.03% of one core, RSS flat at 967 MiB |

That last figure sits just above the provisional idle budget. It is one 30-second sample with a
60 MP image open, so the 500 ms event poll and the window's own redraws are included; it is a
measurement to reproduce and attribute, not an accepted regression.

Crop correctness evidence is rendered, not timed: the `crop` and `crop-draft` smoke scenarios record
correlated state, events and pixel checks, and no latency is claimed from them.

Core figures exclude desktop scheduling, GPU upload and presentation. Reproduce with `editor-performance` and `measure` as described in [development](../engineering/development.md).

Current macOS `measure` runs use background-only bundles to preserve desktop focus. Launch-to-frame timings include copying the executable and creating its temporary bundle; they are background renderer measurements, not foreground activation measurements. Reports identify the launch mode. Earlier launch baselines above predate this wrapper and are not directly comparable.

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

Sampled first-process peak RSS (roughly 50 ms sampling) is 1323 / 1339 MiB p50 / p95
for Z6 and 1975 / 1992 MiB for Fuji. Fuji trial 25 has an unexplained 2436 MiB peak
and a 1038 ms crop update (1007 ms source-to-raster); both tails are retained. Its
pixel, state and reopen checks pass. The capture-heavy workflow cannot isolate
CPU heap, native allocator retention, GPU resources or readback buffers. Separate
screenshot-free live API runs peak at 1583–1647 MiB; the 24-edit run grows only
1.25 MiB after edit three. This does not establish a whole-process bound or prove
absence of leaks. GPU allocations are not measured separately.

Initial development and warm exposure p95 meet their provisional investigation
targets on these files; Fuji memory exceeds the 1536 MiB target. WB redevelopment
is about 0.5 s for Nikon and 1.5–1.6 s for Fuji. The next resource work is to attribute
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

Reproduce with `raw-editor --samples 30` and `editor-performance --samples 30`
through xtask, using the manifest formats in [development](../engineering/development.md).
Local reports retain every trial, percentile input, source/binary hash and failure;
private photographs and captures are not repository assets. These observations
qualify the recorded files and host, not other camera modes or platforms.

## Method

Optimized builds only, with commit, lockfile, OS, CPU/GPU, RAM, display and storage recorded. Report cold and warm runs separately and say which cold is meant. Keep at least 30 samples and never drop failures or tails silently. Measure user event to presented frame, not shader time, and account CPU RSS, cache bytes, GPU allocations and transient copies without double-counting unified memory. Capture idle after all background work stops. No timing gates in CI; CI enforces exactness, deterministic bounds and coverage. VM checks record hypervisor, guest graphics path and software versus accelerated rendering, and never stand in for native timings.

Correctness checks use deterministic fixtures across all EXIF orientations, gradients, patches and fine detail: coordinate round trips, crop coverage, CPU versus GPU differences and export interpretation, including centered Option resizing and angle sweeps that return without cumulative shrinkage. Color and export proofs choose explicit numerical tolerances and viewing conditions.

## Module activation

Logical boundaries and user enablement do not by themselves reduce memory or launch time. After the first external use case is selected, compare the built-in baseline with an activation prototype on the M4: minimal and default configurations, disabled and enabled-but-unused modules, first use, cold and warm launch, RSS, CPU/GPU allocations, idle work and first-use latency, with packaging size reported separately. Disabled modules must start no workers and allocate no processor resources. Reject any optimization that changes output or skips a recipe effect. If the benefit is immaterial, keep the simpler built-in implementation; the external-loader requirement remains regardless.

## Growth rules

Catalog opening never enumerates or decodes all originals. Grid memory depends on visible items and cache quotas. Import, hashing, thumbnailing and indexing use backpressure and resumable batches. Switching photos cancels obsolete preview requests, and the app stays interactive during exports and indexing. Introduce tiling with neighborhood halos before promising unrestricted large-image local processing. A cleanly reported resource limit is acceptable; silent exhaustion is not.
