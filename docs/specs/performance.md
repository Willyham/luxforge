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
| Nikon Z6 NEF, Fujifilm X100VI RAF and DJI Air 2S DNG in the owner's real modes | RAW decode/development, WB redevelopment, history, presentation and peak memory; see the [RAW integration contract](../design/raw-integration.md) |

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

### Sample counts for a p50/p95 claim

Every harness command's default run is a functional run: it proves the journey and gives one launch count you can quote, not a distribution. A p50/p95 figure requires an explicit sample count: 30 samples per recipe for `editor-performance`, 30 inputs for `editor-latency` (one launch), 30 trials per source for `raw-editor`, and at least 5 launches per workload for `measure` — 5 gives a median and a maximum, not a stable p95, so use 30 launches per workload for a p95 claim. Every recorded figure states the count it was taken with.

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
layer inserted where the host places a colour-stage commit, compiled by the real `lightwell.basic`
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
| `analysis::reduce_raster` alone, over an already-rendered raster | 6.9 / 7.7 | 17.4 / 27.5 |
| `query.neutral-sample`: the whole picker, 25 point samples | 0.01 / 0.03 | 0.02 / 0.03 |
| `crop-fit` commit: validation, fitting, compile and persistence, no render | 0.79 (single) | 0.85 (single) |
| Import | 54.4 (single) | 127.3 (single) |
| Reopen: source and preview job after a fresh `EditorService` | 30.5 (single) | 89.9 (single) |

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
exact report. A drafted preview is never analysed — the design keeps the plot labelled stale during
a gesture — so the exact histogram is always reduced from the frame the commit's own refresh
renders, and `analysis_adopted` is the moment that frame and its report are adopted together. The
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
superseded in any run**: a drafted preview is never analysed, so the only reductions are the ones
belonging to committed frames, and each is adopted with the pixels it was reduced from.

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
| Scratch budget high-water mark, 24 MP colour pass | 14 112 000 B (13.46 MiB) of the 64 MiB limit |
| Scratch budget high-water mark, 60 MP colour pass | 13 440 000 B (12.82 MiB) of the 64 MiB limit |
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
the `stale` flag (`crates/lightwell-ui/src/widgets/histogram.rs`,
`crates/lightwell-app/src/view/tools_panel.rs::plot_version`), so a redraw with unchanged bins reuses
the tessellated polygons instead of rebuilding three 256-point fills. Measured again on the same host,
binary SHA-256 `db6ce15c…`, one 30-second sample each: **1.32%** with the 60 MP image open
(`measure --binary target/release/lightwell --output artifacts/idle-after --samples 5`) and **1.38%**
on the 24 MP full-Basic-layer workload (`editor-latency --source fixtures/generated/24mp.jpg --idle
--samples 3 --output artifacts/idle-after-basic`), against 1.29% and 1.46% before the cache. Both are
within a single sample's noise of the unfixed figures, not a resolution. The `histogram` smoke
scenario (`artifacts/idle-histogram`) confirms the cached plot still renders and updates correctly.
The likely dominant cost is not this widget: even a `SyncResult::Unchanged` reply to the 500 ms sync
still drives two `Editor::update` calls and two full `view()` rebuilds of every panel every half
second (`crates/lightwell-app/src/app/mod.rs`, `app/tasks.rs`), and re-tessellating three small
polygons twice a second could not plausibly account for the whole 0.3–0.5 point regression on its
own. That path is outside this change's scope and is being addressed separately. This is a
measurement to attribute, not a resolved regression, and it is reported as a miss below.

### Provisional targets: measured

Each target with the figure that answers it. A miss is a finding for the owner's review, not a
blocker, and no approximate processing, cache or timer was added to reach any of these.

| Provisional target | Measured | Verdict |
| --- | --- | --- |
| Warm 24 MP slider-to-presented-frame p95 below 100 ms | 83.4 ms p95 (74.8 p50, 30 samples) | **Pass** |
| Settled exact histogram p95 below 200 ms after the final input, 24 MP | 107.0 ms p95 (99.7 p50, 30 samples) | **Pass** |
| Scratch aggregate at most 64 MiB | 13.46 MiB high-water at 24 MP, 12.82 MiB at 60 MP | **Pass** |
| 24 MP single-image edit working set ≤ 600 MiB CPU-resident | 645.3 MiB peak in the process that commits the full Basic layer, which also retains two full-window capture readbacks; 568.2 MiB in a second process holding the same committed layer with no captures, settling to 408.0 MiB | **Miss by 45 MiB** on the capturing process, **pass** on the same stack without the harness's captures |
| 60 MP peak ≤ 1 GiB process RSS | 975.0 MiB median peak on a 60 MP open; 1316.4 MiB after sixteen consecutive 60 MP loads | **Pass** on one image, **miss** on the sixteen-load workload (unchanged from before this work: 1316.0 MiB) |
| Idle CPU < 1% of one core over 30 s | 1.32% with a 60 MP image open and 1.38% with a 24 MP full Basic layer, after caching the histogram plot's tessellated geometry; 1.29% / 1.46% for the same two workloads before that cache; 0.93% on the 60 MP workload before the Basic panel and histogram existed at all | **Miss** |
| Geometry input to presented preview p95 < 50 ms once the source preview is ready | not measured for geometry in this round | Open |

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
exact JSON path it came from and a `pass`, `miss` or `not_measured` verdict, next to the one-minute
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

Current macOS `measure` and `editor-latency` runs use background-only bundles to preserve desktop focus, and the editor they launch creates its window invisible. Launch-to-frame timings include copying the executable and creating its temporary bundle; they are background renderer measurements, not foreground activation measurements, and they exclude the cost of placing and compositing a visible window. Nothing in these runs is scanned out, so the presentation figures cover the editor's path to a renderer texture and not what reaching a display would add. Rendering, readback and the state each frame is correlated against are unchanged: the window owns the same Metal surface either way. Reports identify the launch mode and carry the frontmost application before and after every launch. Earlier launch baselines above predate this wrapper and are not directly comparable.

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

The 60 MP current crop has a retained 129.46 ms maximum. Reproduce with
`raw-editor --samples 30` and `editor-performance --samples 30` as above. Local
reports under `artifacts/air2s-editor-30-01/` and `artifacts/air2s-jpeg-*/`
retain every sample, source hash and correlated state; private originals and
captures are excluded from source control. Single-trial Nikon/Fujifilm editor
regressions also pass, separately from the timing distributions.
The host package passes one complete DNG journey with binary SHA-256
`a13a54b0ce2d9c2bf8d7043897988898894931955dfe0a960620623967a2489c`;
its bundled native notices are present and runtime linkage uses no system RAW library.
This is an unsigned macOS development package, not a license audit or platform qualification.

## Method

Optimized builds only, with commit, lockfile, OS, CPU/GPU, RAM, display and storage recorded. Report cold and warm runs separately and say which cold is meant. Keep at least 30 samples and never drop failures or tails silently. Measure user event to presented frame, not shader time, and account CPU RSS, cache bytes, GPU allocations and transient copies without double-counting unified memory. Capture idle after all background work stops. No timing gates in CI; CI enforces exactness, deterministic bounds and coverage. VM checks record hypervisor, guest graphics path and software versus accelerated rendering, and never stand in for native timings.

Correctness checks use deterministic fixtures across all EXIF orientations, gradients, patches and fine detail: coordinate round trips, crop coverage, CPU versus GPU differences and export interpretation, including centered Option resizing and angle sweeps that return without cumulative shrinkage. Color and export proofs choose explicit numerical tolerances and viewing conditions.

## Module activation

Logical boundaries and user enablement do not by themselves reduce memory or launch time. After the first external use case is selected, compare the built-in baseline with an activation prototype on the M4: minimal and default configurations, disabled and enabled-but-unused modules, first use, cold and warm launch, RSS, CPU/GPU allocations, idle work and first-use latency, with packaging size reported separately. Disabled modules must start no workers and allocate no processor resources. Reject any optimization that changes output or skips a recipe effect. If the benefit is immaterial, keep the simpler built-in implementation; the external-loader requirement remains regardless.

## Growth rules

Catalog opening never enumerates or decodes all originals. Grid memory depends on visible items and cache quotas. Import, hashing, thumbnailing and indexing use backpressure and resumable batches. Switching photos cancels obsolete preview requests, and the app stays interactive during exports and indexing. Introduce tiling with neighborhood halos before promising unrestricted large-image local processing. A cleanly reported resource limit is acceptable; silent exhaustion is not.
