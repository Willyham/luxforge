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
| Slider input to presented frame at Fit, warm 24 MP | p95 < 16 ms; acceptable below 32 ms; a miss at or above 32 ms (owner's target of 2026-09-22) |
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

### The masked colour primitive, its own run

A masked colour layer costs the units it would have cost unmasked, plus one coverage evaluation and
one blend per pixel **inside the mask's bounds rectangle**, and nothing at all outside it. Measured on
the host above, release, single invocation, three measured renders after one warm pass, over a
programmatically filled 6000 × 4000 frame with one `+1 EV` exposure unit
(`render::tests::masked_colour_cost_on_a_24_megapixel_frame`, an ignored measurement test):

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
`render::tests::a_masked_operation_evaluates_no_unit_outside_its_bounds`, which requires the count to
equal the rectangle's area exactly.

`editor-performance` on 24 MP, 30 samples, after the change: colour baseline 33.1 / 38.3 ms and one
`+1 EV` Basic layer 40.3 / 45.5 ms, both inside the recorded ranges above, on a host whose load was
shared with other sessions. There is no paired before-run from this worktree; the unmasked path's
arithmetic is unchanged by construction and proved byte-identical by the colour tests, and the mask is
consulted once per operation per row rather than per pixel.

### The masked spatial primitive, one to four layers

A masked spatial layer costs what it would have cost unmasked, plus one coverage evaluation and one
blend per pixel of the tiles the mask's bounds rectangle reaches, minus the whole unit chain of every
tile it does not. Each spatial layer, masked or not, is a stage boundary and therefore a **sequential
full frame**: the design caps masked ones at four for that reason, and the host now refuses a fifth
with a `resource-limit` error naming the limit.

`cargo test --release --locked --package lightwell-core --lib -- --ignored masked_spatial_timing
--nocapture` (`render::spatial::tests::masked_spatial_timing`), on the host recorded above, on
in-memory synthetic frames rendered by the core alone, warm source and warm estimate store, p50 and
the slowest of 5 runs, one `lightwell.presence` clarity `+100` layer per mask. "Whole frame" is a
gradient whose bounds rectangle is the entire stage; "right-edge band" is one confined to about a
tenth of the columns. The tile counts are exact counters read from the host
(`masked_tile_counts`), not estimates. The load average rises during the run, because the render
drives the whole Rayon pool; the figure quoted is the one before it starts, which is the other
sessions' load and the only part of it a measurement can be spoiled by.

**These rows replace the contended first measurement.** The whole test was run twice back to back
on a quiesced host, one-minute load average 3.32 before the first pass and 5.12 and 6.81 before the
two halves of the second, against the 87.9 the first measurement was taken under. Both passes are
given, because the spread between two identical measurements is the only honest statement about how
quotable a millisecond from this machine is.

| Stage | Layers | Mask | p50 / slowest ms, pass 1 | p50 / slowest ms, pass 2 | Tiles copied / evaluated per render | Budget peak |
| --- | --- | --- | --- | --- | --- | --- |
| 6000 × 4000 | 1 | none | 181 / 187 | 217 / 221 | 0 / 0 | 242.5 MiB |
| 6000 × 4000 | 1 | whole frame | 204 / 206 | 243 / 261 | 0 / 96 | 255.3 MiB |
| 6000 × 4000 | 2 | whole frame | 408 / 424 | 449 / 458 | 0 / 192 | 255.3 MiB |
| 6000 × 4000 | 3 | whole frame | 693 / 746 | 736 / 756 | 0 / 288 | 255.3 MiB |
| 6000 × 4000 | 4 | whole frame | 1088 / 1991 | 905 / 919 | 0 / 384 | 255.3 MiB |
| 6000 × 4000 | 1 | none | 365 / 460 | 200 / 201 | 0 / 0 | 242.5 MiB |
| 6000 × 4000 | 1 | right-edge band | 166 / 168 | 153 / 162 | 80 / 16 | 255.3 MiB |
| 6000 × 4000 | 2 | right-edge band | 306 / 323 | 304 / 318 | 160 / 32 | 255.3 MiB |
| 6000 × 4000 | 3 | right-edge band | 462 / 467 | 468 / 472 | 240 / 48 | 255.3 MiB |
| 6000 × 4000 | 4 | right-edge band | 616 / 622 | 642 / 741 | 320 / 64 | 255.3 MiB |
| 10000 × 6000 | 1 | none | 654 / 663 | 863 / 884 | 0 / 0 | 249.9 MiB |
| 10000 × 6000 | 1 | whole frame | 809 / 816 | 831 / 852 | 0 / 240 | 239.7 MiB |
| 10000 × 6000 | 2 | whole frame | 1648 / 1651 | 1654 / 1699 | 0 / 480 | 239.7 MiB |
| 10000 × 6000 | 3 | whole frame | 2545 / 2800 | 2550 / 2743 | 0 / 720 | 239.7 MiB |
| 10000 × 6000 | 4 | whole frame | 3354 / 3480 | 3263 / 3269 | 0 / 960 | 239.7 MiB |
| 10000 × 6000 | 1 | none | 680 / 684 | 687 / 693 | 0 / 0 | 249.9 MiB |
| 10000 × 6000 | 1 | right-edge band | 407 / 416 | 394 / 413 | 204 / 36 | 239.7 MiB |
| 10000 × 6000 | 2 | right-edge band | 807 / 810 | 783 / 786 | 408 / 72 | 239.7 MiB |
| 10000 × 6000 | 3 | right-edge band | 1196 / 1208 | 1179 / 1198 | 612 / 108 | 239.7 MiB |
| 10000 × 6000 | 4 | right-edge band | 1592 / 1613 | 1593 / 1694 | 816 / 144 | 239.7 MiB |

**Scope.** The quiesced figures are in the same place as the delivered unmasked quiesced figure for
the same operation (191 / 202 ms at 24 MP), which is what says the host was quiet: three of the four
measurements of the identical unmasked 24 MP work read 181, 200 and 217 ms, against 353 and 543 ms
when the same rows were taken at load 87.9. **One of the four read 365 ms and is an outlier**, and
the 24 MP four-layer pass-1 slowest of 1991 ms against a pass-2 slowest of 919 ms is another; this
machine is shared and a stray minute still lands in a five-sample window. The masked rows themselves
are steady — every masked pair above agrees to within 10% except the 24 MP four-layer row — so these
are quotable as the cost of a masked Presence layer, with that spread stated.

- **Cost grows with the layer count, linearly.** At 60 MP whole frame the four masked rows are 809,
  1648, 2545, 3354 ms: 809 ms per layer, flat. At 24 MP they are 204, 408, 693, 1088. That is what a
  sequential full frame per layer predicts, and it is why four is the cap: a fourth masked spatial
  layer at 60 MP costs the person three and a third seconds of exact render, and it is the *exact*
  phase, behind the proxy, so it is not what the hand feels.
- **A tile outside the bounds rectangle costs no unit evaluation**, exactly: 204 of 240 tiles copied
  at 60 MP and 80 of 96 at 24 MP, so a small mask is *cheaper* than the unmasked layer — 394–407 ms
  against 654–863 at 60 MP, 153–166 against 181–217 at 24 MP, a saving of about 40% and 20%. That is
  a counter rather than an inference.
- **A whole-frame mask costs 12–13% over no mask at one layer at 24 MP** (204 against 181 in pass 1,
  243 against 217 in pass 2). At 60 MP the two passes disagree — 809 against 654 is +24%, 831
  against 863 is −4% — so at 60 MP the honest statement is that the whole-frame mask costs
  **somewhere between nothing and a quarter** of the unmasked layer at one layer, and the 24 MP
  figure is the quotable one. The structural part of it is the working set: a masked tile holds one
  extra tile-sized plane, the snapshot the blend is against, which at 60 MP moves the plan's
  concurrency from 8 tiles to 7.
- The blend is **in place** in the last unit's planes. An earlier spelling that copied the tile out
  and blended into a second buffer measured 4464 ms against 1873 at 60 MP — a 2.4× overhead from two
  fresh tile-sized allocations per tile, not from arithmetic. That spelling is not what shipped, and
  it is recorded because it is the trap: the blend is cheap and the allocations were not.

One further cost is arithmetic from the delivered retained-frame rule rather than a measurement, and
is a finding for the owner: on the **RAW linear path** each spatial operation materializes one `f32`
frame and every one of them is retained for the whole render, so four masked spatial layers at 60 MP
retain about 2.9 GB of float frames. The byte path has no equivalent cost because its frames are
sequential and dropped.

### The masked spatial primitive, a mask of many components

The other half of the same cost: one masked Presence layer whose mask holds 1, 4, 16 and 32
components — [the limit](../design/masking.md) — each a linear gradient across the whole frame, so
the bounds rectangle is the whole stage and **every** component is evaluated at every pixel. The
modes cycle through add, subtract and intersect, because those are one `max` and two `min`s per
pixel and nothing else. `cargo test --release --locked --package lightwell-core --lib -- --ignored
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

### Masks in the proxy phase

A masked recipe is proxy eligible by construction: mask geometry is stored normalized, so the mask
compiled against the proxy stage is the same field at a smaller scale and the proxy frame is the
exact recipe at proxy size. The only thing that changes with the stage is sampling, and the recorded
default ([masking](../design/masking.md#point-queries-and-proxies), proposal P5) supersamples the
**mask field only**, 2 × 2 per pixel, when the mask's narrowest feature is under two pixels of the
proxy stage.

Core cost of the proxy render a drag presents, measured on the host above, release, 25 measured
renders after one warm pass against a cached proxy source, display bounds 2880 × 1800
(`proxy::tests::measure_the_masked_proxy_render_on_photo_sized_sources`, an ignored measurement
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
target at the median by about a millisecond. This is the proxy phase, which is what the hand feels;
the exact phase behind it is the core table above.

`editor-performance --samples 30` was re-run on the same host beside the first, contended drags,
release, warm cache, on `24mp.jpg` (load average 8.6 rising to 17.3) and `60mp.jpg` (load average
17.3 rising to 20.8). Both **passed every check**, including that the catalog reopen reconstructs the
original historical state and the source SHA-256 is unchanged, so the phase leaves the core's own
correctness diagnostics intact. Their timings are not quoted, for the reason the drags were not: the
harness's own full-Basic core render read 128.2 ms p50 / 224.5 ms p95 at 24 MP and 527.1 / 619.1 ms
at 60 MP, and a p95 more than 1.7 times its own p50 in a warm 30-sample loop is a measure of the
host's queue, not of the render.

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

### Instant previews: proxy phase, hop rule and the surface primitive

Native Apple M4 Pro, macOS 26.5.2, Metal, a 2880 × 1800 physical window at 2× scale, release
builds, background hidden-window launches, warm filesystem cache, one-minute load averages between
4 and 7.5 on a shared host. Application SHA-256 `16423973…`. These rows supersede the
slider-to-presented-frame and queue-cancellation tables above, which measured the full-resolution
render-and-upload path that no longer exists at Fit; the histogram, memory and launch rows above
still describe the current build unless restated here. Presented now means the update in which the
frame became the photo surface's source; it is drawn by the redraw that update requests, the next
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

The figures no longer depend on the source size, because every frame in a drag is the proxy phase:
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
round trip is synchronous and takes 0.18 / 0.19 ms), the queue keeps one proxy job active and one
pending, and every superseded exact phase is cancelled (160, 124 and 124 of them in the three runs).
Before this work the same burst presented one frame in three seconds: every render finished after
a newer job had been requested and was dropped as stale.

| Settled exact histogram after the last input (p50 / p95 ms) | Figure |
| --- | --- |
| 24 MP, exposure only, commit mode, 30 samples | 60.1 / 70.3 |
| 24 MP, exposure only, drag mode, 2 commits | 54.3 / 54.3 |
| 24 MP, full Basic layer, 2 commits | 153.8 / 158.6 |
| 60 MP, full Basic layer, 2 commits | 356.4 / 367.0 |

The settled histogram is the exact phase of the committed frame: the full-resolution render with
every unit, then the reduction. It is not on the input path, so a drag does not wait for it; the
60 MP full-Basic figure misses the 200 ms threshold that was set for 24 MP and is recorded here
because the design asks for the tails.

Where the per-input time went before the last two changes, measured with the per-leg timings the
`slider_draft_preview` event now records: the owner's `draft.set` and preview-job work take under
0.2 ms, the desktop's update, model derivation and view under 0.15 ms together, and every message
handed back into the update loop through the runtime arrived about 7.9 ms later, one frame of the
120 Hz display, because a redraw is always in flight during a drag. With the round trip as a task
and the frame through an image allocation, a 24 MP drag measured 38.6 / 63.0 ms; with the round
trip synchronous, 30.2 / 58.7; with the surface primitive, the rows above.

| Core diagnostic, 24 MP crop stack (p50 / p95 ms, 10 samples) | Full resolution | Proxy for 2880 × 1800 |
| --- | --- | --- |
| Same stack without colour | 29.1 / 32.9 | 16.9 / 19.0 |
| One +1 EV Basic layer | 35.8 / 37.5 | 21.1 / 23.0 |
| Full Basic layer | 77.3 / 79.7 | 46.9 / 48.0 |
| Proxy build (a cache miss: once per source, bounds and window size) | — | 21.2 / 25.7 |

The proxy source for this stack is 4677 × 3118, larger than the 2879 × 1618 output it produces,
because the 16:9 crop discards most of the rotated stage; the colour pass now covers only the band
of rows the crop reads, which is what brought the full-Basic rows down from 162.5 and 99.8 ms in the
first measurement of this plan. The full-Basic proxy render remains the largest per-input cost and
is listed in the [performance rules](../engineering/performance-rules.md#known-remaining-costs).

RAW, one functional trial per camera through `raw-editor` (the same 13-step journey as the RAW
tables below, so these are single launches and not distributions): request to display of the
exposure step is 10.5 ms on the Z6, 14.6 ms on the X100VI and 15.0 ms on the Air 2S, against
133.3, 178.8 and the Air 2S figures recorded below for the full-resolution path; rotate, crop-fit
and undo present in 18 to 46 ms. The white-balance steps still take 354–364 ms on the Z6 and
1364–1428 ms on the other two, because temperature, tint, the gains and the neutral pick redevelop
the mosaic on the source worker before any proxy exists; that is the RAW white-balance drag listed
in the [performance rules](../engineering/performance-rules.md#known-remaining-costs).

Memory and idle from the same timing tier, five launches per workload: sampled peak RSS 130.4 MiB
empty, 389.7 MiB at 24 MP and 808.8 MiB at 60 MP (medians); idle CPU 1.53% of one core over 30 s
with the 60 MP image open, a miss of the 1% target in the same range as the 1.29–1.46% recorded
before this work, with the histogram and the surface primitive both drawn on each redraw and the
500 ms sync still in place; no timer was added and none remains for previews or gestures.

### Provisional targets: measured

Each target with the figure that answers it. A miss is a finding for the owner's review, not a
blocker, and no approximate processing, cache or timer was added to reach any of these.

| Provisional target | Measured | Verdict |
| --- | --- | --- |
| Warm 24 MP slider-to-presented-frame p95 below 16 ms, acceptable below 32 ms | 29.0 ms p95 (9.5 p50, 30 samples); 24.8 ms p95 with a full Basic layer | **Acceptable** (measured against the earlier 100 ms threshold; below the 32 ms bound, not the 16 ms target) |
| Instant preview: drained drag p95 ≤ 33 ms at Fit, 24 and 60 MP, full Basic layer, with and without a 7° crop | 24.8, 28.9 and 28.7 ms p95 (30 samples each) | **Pass** |
| Instant preview: burst drag ≥ 30 presented frames per second | 53.7 (exposure), 41.7 and 42.5 (full Basic, 24 and 60 MP) | **Pass** |
| Instant preview: burst staleness p95 ≤ 50 ms | 32.9, 34.1 and 34.1 ms | **Pass** |
| Instant preview: RAW exposure step presented within 50 ms | 10.5 / 14.6 / 15.0 ms request to display on the Z6 / X100VI / Air 2S, one trial each; a drained-drag distribution on RAW is not measured yet | **Pass** (functional) |
| Settled exact histogram p95 below 200 ms after the final input, 24 MP | 70.3 ms p95 (60.1 p50, 30 samples) exposure only; 158.6 ms with a full Basic layer (2 commits) | **Pass** |
| Scratch aggregate at most 64 MiB | 13.46 MiB high-water at 24 MP, 12.82 MiB at 60 MP | **Pass** |
| 24 MP single-image edit working set ≤ 600 MiB CPU-resident | 645.3 MiB peak in the process that commits the full Basic layer, which also retains two full-window capture readbacks; 568.2 MiB in a second process holding the same committed layer with no captures, settling to 408.0 MiB | **Miss by 45 MiB** on the capturing process, **pass** on the same stack without the harness's captures |
| 60 MP peak ≤ 1 GiB process RSS | 975.0 MiB median peak on a 60 MP open; 1316.4 MiB after sixteen consecutive 60 MP loads | **Pass** on one image, **miss** on the sixteen-load workload (unchanged from before this work: 1316.0 MiB) |
| Idle CPU < 1% of one core over 30 s | 1.32% with a 60 MP image open and 1.38% with a 24 MP full Basic layer, after caching the histogram plot's tessellated geometry; 1.29% / 1.46% for the same two workloads before that cache; 0.93% on the 60 MP workload before the Basic panel and histogram existed at all | **Miss** |
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

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Metal, release `--locked`, warm source cache, background bundle launches for every desktop figure; core figures are in-memory synthetic frames rendered by the core alone, warm, with the estimate store warm where a global estimate exists. Every desktop figure ends at the renderer's `Uploaded` callback, not display scanout. All three modules run the same full-resolution draft path Basic runs: nothing approximate, no extra cache and no timer was added to reach any figure, and each miss below is a finding for the owner's review.

### Core cost of the units

`cargo test --release -- --ignored presence_timing` and the spatial primitive's own timing test, p50 / p95 over 10 runs, one operation over a textured frame. Working set is one tile's reserved bytes; concurrency is how many tiles the 256 MiB spatial budget allowed in flight at once.

| Stage | Operation | p50 / p95 ms | Summed halo | Working set | Concurrency | Budget peak |
| --- | --- | --- | --- | --- | --- | --- |
| 6000 × 4000 | Texture +100 | 212 / 225 | 8 px | 14.7 MiB | 14 | 205.8 MiB |
| 6000 × 4000 | Clarity +100 | 191 / 202 | 199 px | 20.2 MiB | 12 | 242.5 MiB |
| 6000 × 4000 | Dehaze +100 | 129 / 133 | 67 px | 11.0 MiB | 14 | 154.1 MiB |
| 6000 × 4000 | All three +100 | 1670 / 1708 | 274 px | 61.3 MiB | 4 | 245.3 MiB |
| 10000 × 6000 | Texture +100 | 588 / 609 | 14 px | 15.2 MiB | 14 | 213.3 MiB |
| 10000 × 6000 | Clarity +100 | 690 / 731 | 327 px | 31.2 MiB | 8 | 249.9 MiB |
| 10000 × 6000 | Dehaze +100 | 339 / 353 | 107 px | 13.1 MiB | 14 | 183.5 MiB |
| 10000 × 6000 | All three +100 | 12447 / 12561 | 448 px | 101.1 MiB | 2 | 202.1 MiB |
| 6000 × 4000 | Host box blur r = 137 (test unit, naive) | 2528 / 2694 | 137 px | 17.1 MiB | 14 | 240.0 MiB |
| 10000 × 6000 | Host box blur r = 224 (test unit, naive) | 14970 / 15074 | 224 px | 24.1 MiB | 10 | 240.9 MiB |

The three units together cost about eight times the sum of the singles. That is structural, not a hot loop: with a summed halo of 448 px a 512 px tile reads a 1408 px input region, dehaze fills 1194 px and texture 1166 px of it to deliver 512 px, and the 101 MiB working set cuts concurrency to two tiles. Larger tiles amortise the halo better but a 2048 px tile's input region does not fit the spatial budget with the frozen declarations; tiling each unit separately would need an intermediate frame between units. Both are open proposals for the owner, with these figures as the baseline. The vignette's unit alone, single-threaded over 6000 × 4000: 57 ms at amount −50, 505 ms at +50 (the positive branch encodes and decodes each channel), 164 ms at roundness −100.

### Desktop slider-to-presented-frame

`editor-latency --mode drag` on the generated 24 MP fixture, 30 drained inputs each, through the new `--action` and `--parameter` selector. The Basic exposure figure in the same harness is 74.8 / 83.4 ms.

| Slider (24 MP, p50 / p95 ms) | Input to presented frame | Settled exact histogram | Peak RSS | Verdict (16 ms target, 32 ms bound) |
| --- | --- | --- | --- | --- |
| Colour mixer, Red hue | 157.1 / 216.1 (min 137.0, max 342.4) | 280.3 / 287.3 | 1357 MiB | **Miss** |
| Vignette, Amount | 121.6 / 135.3 (min 112.0, max 147.9) | 125.0 / 201.4 | 1352 MiB | **Miss** |
| Presence, Clarity | 211.2 / 227.9 | not measured | 1156 MiB | **Miss** |
| Presence, Texture | 230.1 / 244.6 | not measured | 993 MiB | **Miss** |
| Presence, Dehaze | 157.2 / 178.0 | not measured | 1056 MiB | **Miss** |

The mixer's per-pixel cost is the Oklab conversion (three cube roots each way) that Basic's saturation and vibrance units already pay, now paid a second time for a second unit; the vignette's is the extended encode and decode of every channel in its positive branch and the per-pixel mask. Neither exceeds the per-frame cost of the crop resample the earlier rows record, and both stay well under the 1.5 GiB RSS investigation target. The three Presence sliders missed as the design anticipated when each draft rendered the whole 24 MP stage through a tiled neighbourhood operation. These rows predate the instant-preview merge; since it, a Presence stack at Fit renders through the display-bounded proxy and is marked approximate, and the rows are re-measured below.

### After the instant-preview merge

The same harness on the same fixture after main's instant previews were merged (22 September 2026): at Fit every drafted frame is the display-bounded proxy render, so the input-to-presented figure is the proxy phase, and the exact phase runs behind it for the histogram, the overlays and the 100% view. The Presence frames carry `proxy_approximate: true` in the event log (a spatial layer's neighbourhoods scale with the stage); the mixer and vignette frames are proxy renders that equal the exact recipe at their own scale.

| Slider (24 MP, p50 / p95 ms) | Input to presented proxy frame | Settled exact histogram | Peak RSS | Verdict (16 ms target, 32 ms bound) |
| --- | --- | --- | --- | --- |
| Presence, Clarity | 16.7 / 17.3 | 216.1 / 227.8 | 989 MiB | **Acceptable** |
| Presence, Texture | 22.7 / 26.6 | 266.0 / 271.7 | 912 MiB | **Acceptable** |
| Presence, Dehaze | 16.5 / 17.4 | 159.9 / 164.4 | 908 MiB | **Acceptable** |
| Colour mixer, Red hue | 22.2 / 28.3 | 182.5 / 186.3 | 1267 MiB | **Acceptable** |
| Vignette, Amount | 14.9 / 22.9 | 66.5 / 130.1 | 1261 MiB | **Acceptable** |

The settled-histogram column is the exact phase's cost and stays where the full-resolution rows above put it, because that is the same 24 MP render; it no longer stands between an input and the frame on screen. Under the owner's target of 2026-09-22 (16 ms p95, acceptable below 32 ms) every slider of the three modules is inside the acceptable bound and none meets the 16 ms target yet; the Presence rows improved by about a factor of ten over their pre-merge rows. Clarity and Dehaze at 17.3 and 17.4 ms p95 sit just past the target, so the display-bounded proxy render of a spatial layer is the next cost to measure and cut, with the coarser-proxy-while-moving and GPU colour-stage proposals of the [instant-preview design](../design/instant-preview.md#proposals-and-later-work) as the candidates.


Rendered evidence is the `presence`, `mixer` and `vignette` smoke scenarios (15, 8 and 12 correlated frames at Fit and 100% with the module's own controls visible), and the acceptance chapter's ten checks per module through the JSON method table. A reviewer's render of the owner's 14 MP Sapa drone JPEG through the core alone (release, in memory: dehaze 65 ms, clarity 104 ms, texture 127 ms, all three at +50 672 ms) showed Dehaze +60 and +100 lifting the veil and deepening colour plausibly, Clarity +100 adding local contrast without visible halos at fit and at 100%, and Texture +100 sharpening fine detail with the expected crunch; it is a visual check, not a measurement. On a synthetic haze-free flat field Dehaze +100 drives the field toward black, because the dark-channel prior reads a uniform patch darker than the atmosphere as pure veil and the frozen `OMEGA_MAX = 1` removes all of it; the study records this and real photographs, whose windows contain dark pixels, do not show it.

## Brush-heavy recipes across history

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Rust 1.94.0, release `--locked`, warm filesystem cache, catalog on the internal APFS SSD. One process per fixture:

```text
LIGHTWELL_MASK_GROWTH_SOURCE=fixtures/generated/24mp.jpg /usr/bin/time -l \
  cargo test --release --package lightwell-core --lib measure_mask_growth -- --ignored --nocapture
```

Scope: `lightwell-core`'s own catalog, one stroke per history entry written through the production write path, each stroke captured at 100 positions and decimating to 67–78 stored ones, packed into the densest mask table the declared limits admit. There is no `mask.*` command that posts a stroke yet, so the session is built at the recipe and entry level rather than through the API; the bytes it writes are the bytes the API path will write, because it is the same `insert_entry`. The [stroke-storage table](../design/masking.md#stroke-storage) measures 200 strokes packed 64 to a mask rather than 81, which is the same curve one arrangement less dense: 1.11 MB there against 1.10 MB here.

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

## Preset import parse

Native Apple M4 Pro (14 cores, 48 GiB), macOS 26.5.2, Rust 1.94.0, release `--locked`, in memory, 20 runs each: `cargo test --release --package lightwell-core --lib measure_preset_parse -- --ignored --nocapture`. Each synthetic document is filled to the 1 MiB request limit in the shape that presses one bound, and `inspect_preset` runs detection, parsing, mapping and the report. It reads no file and renders nothing.

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

## Method

Optimized builds only, with commit, lockfile, OS, CPU/GPU, RAM, display and storage recorded. Report cold and warm runs separately and say which cold is meant. Keep at least 30 samples and never drop failures or tails silently. Measure user event to presented frame, not shader time, and account CPU RSS, cache bytes, GPU allocations and transient copies without double-counting unified memory. Capture idle after all background work stops. No timing gates in CI; CI enforces exactness, deterministic bounds and coverage. VM checks record hypervisor, guest graphics path and software versus accelerated rendering, and never stand in for native timings.

Correctness checks use deterministic fixtures across all EXIF orientations, gradients, patches and fine detail: coordinate round trips, crop coverage, CPU versus GPU differences and export interpretation, including centered Option resizing and angle sweeps that return without cumulative shrinkage. Color and export proofs choose explicit numerical tolerances and viewing conditions.

## Module activation

Logical boundaries and user enablement do not by themselves reduce memory or launch time. After the first external use case is selected, compare the built-in baseline with an activation prototype on the M4: minimal and default configurations, disabled and enabled-but-unused modules, first use, cold and warm launch, RSS, CPU/GPU allocations, idle work and first-use latency, with packaging size reported separately. Disabled modules must start no workers and allocate no processor resources. Reject any optimization that changes output or skips a recipe effect. If the benefit is immaterial, keep the simpler built-in implementation; the external-loader requirement remains regardless.

## Growth rules

Catalog opening never enumerates or decodes all originals. Grid memory depends on visible items and cache quotas. Import, hashing, thumbnailing and indexing use backpressure and resumable batches. Switching photos cancels obsolete preview requests, and the app stays interactive during exports and indexing. Introduce tiling with neighborhood halos before promising unrestricted large-image local processing. A cleanly reported resource limit is acceptable; silent exhaustion is not.
