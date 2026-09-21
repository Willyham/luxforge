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
| Nikon Z6 NEF and Fujifilm X100VI RAF in the owner's real modes | Later RAW decode quality and peak memory |

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

With the Basic module's Exposure parameter, `editor-performance` on 24 and 60 MP, 30 samples each,
release, warm cache, on the M4 Pro; core render only, with no desktop scheduling, GPU upload or
presentation. Each row renders the same recipe through `render`, so the difference between the last
two rows is the streamed pointwise colour pass alone: the same frames are materialized either way.

| Measurement | 24 MP | 60 MP |
| --- | --- | --- |
| Identity recipe (shared source buffer, no frame) | under 0.01 ms | under 0.01 ms |
| The 200-transform and 10° crop stack, no colour layer (p50 / p95) | 35.2 / 42.4 ms | 85.6 / 142.4 ms |
| The same stack with one `+1 EV` Basic layer (p50 / p95) | 56.3 / 123.4 ms | 129.4 / 221.3 ms |

The colour pass costs about 21 ms at 24 MP and 44 ms at 60 MP at the median. Both p95 tails are far
above their medians (123 ms and 221 ms) because the run materializes two photo-sized frames per
sample and the tail follows the allocator, not the pass; the slider-to-presented-frame threshold in
the [Basic design](../design/basic-and-histogram.md#resource-and-responsiveness-constraints) is a
desktop measurement that this core-only diagnostic does not make. Recorded here with that scope; no
threshold is claimed met or missed from these numbers alone.

With the Basic module's five Tone fields (Contrast, Highlights, Shadows, Whites, Blacks) added,
`editor-performance` on 24 and 60 MP, 30 samples each, release, warm cache, on the M4 Pro; core
render only. The same 200-transform-and-10°-crop stack as the row above, with one Basic layer
holding `exposure` and all five Tone fields non-neutral, compiled by the real `lightwell.basic`
module into two real pointwise units (`Exposure`, then `Tone`) run as one streamed colour pass.

| Measurement | 24 MP | 60 MP |
| --- | --- | --- |
| The 200-transform and 10° crop stack, no colour layer (p50 / p95) | 37.3 / 46.0 ms | 135.1 / 274.6 ms |
| The same stack with one `+1 EV` Basic layer, exposure only (p50 / p95) | 53.1 / 60.8 ms | 129.2 / 148.5 ms |
| The same stack with `+1 EV` exposure and all five Tone fields (p50 / p95) | 95.7 / 139.1 ms | 235.0 / 264.7 ms |

The Tone unit's own added cost over the exposure-only row is about 43 ms at the 24 MP median and
about 106 ms at the 60 MP median; both rows compile a second `PointwiseColor` unit into the same
streamed colour operation, so this reflects the `Tone` unit's own per-pixel work (encode/decode
through the extended sRGB transfer function plus the three composed curve stages), not a second
render or a second frame allocation. As with the exposure-only row, this is a core-only diagnostic
without desktop scheduling, GPU upload or presentation; no responsiveness threshold is claimed met
or missed from these numbers alone. The baseline row's own run-to-run variance (24 MP p50 37.3 ms
here versus 35.2 ms in the row above, both from independent `editor-performance` invocations on the
same host) is a useful reminder that these are diagnostic samples, not a controlled A/B on identical
process state.

With Vibrance and Saturation added to the Basic module (TASK-016) as two separate `PointwiseColor`
units, the same diagnostic on 24 and 60 MP, 30 samples each, release, warm cache, M4 Pro, measured
against the same `colour_baseline_same_stack_without_colour` row on that run (32.7 / 35.1 ms at
24 MP, 87.6 / 100.9 ms at 60 MP; run-to-run variance against the table above, same scope: core
render only, no desktop scheduling, GPU upload or presentation), gave a `vibrance: 50, saturation:
20` Basic layer (two Oklab units, one per parameter) a p50/p95 of 150.8 / 163.2 ms at 24 MP and
402.3 / 594.1 ms at 60 MP — a colour-pass cost, against that run's own baseline, of about 118 ms at
24 MP and 315 ms at 60 MP, against about 17 ms and 33 ms for the single-unit `+1 EV` exposure pass
measured the same way. Each Oklab unit round-trips every pixel through `to_oklab`/`from_oklab` once
(two signed cube roots and two 3×3 matrix products each way), so the two separate units cost close
to double one exposure unit's single multiply-only pass, with a second, avoidable round trip: the
saturation unit re-converts the pixel vibrance already adjusted, discarding and immediately
recomputing values the vibrance unit already held.

Vibrance and Saturation were subsequently fused into one `ColourAdjust` unit (`colour.rs`): it
converts to Oklab once, computes vibrance's chroma-/hue-dependent gain and saturation's uniform
gain from that one conversion, scales `a`/`b` by their combined factor, and converts back once,
mathematically identical to the sequential pair (`docs/design/basic-colour.md`'s frozen equations
are unchanged; the two are within `1e-6` to `2.5e-5` of each other depending on how far out of
gamut the input is — see `colour_adjust_matches_the_sequential_pair_within_the_frozen_tolerance` in
`colour.rs`) but without the second round trip. Re-measured the same way, 30 samples each, release,
warm cache, M4 Pro (host under heavy concurrent load from other sessions at measurement time — load
average around 14–20 on a 14-core M4 Pro, so these figures carry more run-to-run noise than usual,
particularly at 60 MP; a second back-to-back run's 60 MP delta ranged from 169 ms to 233 ms against
the same 118 ms/315 ms before-figures above):

| Measurement | 24 MP | 60 MP |
| --- | --- | --- |
| Before: two separate Oklab units (p50 / p95) | 150.8 / 163.2 ms | 402.3 / 594.1 ms |
| After: one fused `ColourAdjust` Oklab unit (p50 / p95) | 107.6 / 112.4 ms | 357.8 / 440.3 ms |

Against each run's own baseline (32.7 ms / 87.6 ms before; 31.7 ms / 188.8 ms after — the 60 MP
baseline itself moved between runs under the load noted above, which is why the delta below, not
the raw after-figure, is the comparable number), the colour pass's own added cost dropped from about
118 ms to about 76 ms at 24 MP (roughly a third less) and from about 315 ms to somewhere in the
169–233 ms range at 60 MP across repeated runs (roughly a quarter to close to half less), consistent
with removing one of the two round trips through the independently rounded inverse matrices while
keeping the same per-pixel vibrance weight computation. The 60 MP p95 sits well above its median for
the same allocator-tail reason noted above; no desktop responsiveness threshold is claimed met or
missed from this core-only number.

With the Basic module's Temperature and Tint, `editor-performance` re-run on 24 and 60 MP, 30 samples
each, release, warm cache, on the M4 Pro; core render only, with no desktop scheduling, GPU upload or
presentation. The baseline and the exposure row were re-measured in the same run, so the three colour
rows are directly comparable to each other; they are faster than the exposure figures recorded above,
which came from a separate run, so compare rows within a run and not across runs.

| Measurement | 24 MP | 60 MP |
| --- | --- | --- |
| The 200-transform and 10° crop stack, no colour layer (p50 / p95) | 31.8 / 34.4 ms | 68.9 / 73.8 ms |
| The same stack with one `+1 EV` Basic layer (p50 / p95) | 46.6 / 49.9 ms | 110.0 / 126.3 ms |
| The same stack with one `temperature 30, tint −10` Basic layer (p50 / p95) | 50.9 / 54.9 ms | 122.4 / 133.3 ms |
| `query.neutral-sample`: the whole picker, 25 point samples (p50 / p95) | 0.013 / 0.024 ms | 0.015 / 0.027 ms |

The white-balance pass costs about 19 ms at 24 MP and 54 ms at 60 MP at the median, roughly 4 ms and
12 ms more than exposure's single scalar multiply on the same frames: the unit is one composite 3 × 3
linear-sRGB matrix per pixel, folded once in `f64` at compile time and applied in `f32`, against
exposure's one multiply. The neutral picker is not a frame operation at all. It evaluates 25 point
samples of the stage the Basic layer receives, each at `O(layers)` through the compiled stack, and
allocates no frame, so a pick costs about 0.015 ms on the catalog owner at either size and does not
grow with the image. Its cost grows with the stack depth, not the pixel count. Recorded with that
scope; no threshold is claimed met or missed from these numbers alone.

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

## Method

Optimized builds only, with commit, lockfile, OS, CPU/GPU, RAM, display and storage recorded. Report cold and warm runs separately and say which cold is meant. Keep at least 30 samples and never drop failures or tails silently. Measure user event to presented frame, not shader time, and account CPU RSS, cache bytes, GPU allocations and transient copies without double-counting unified memory. Capture idle after all background work stops. No timing gates in CI; CI enforces exactness, deterministic bounds and coverage. VM checks record hypervisor, guest graphics path and software versus accelerated rendering, and never stand in for native timings.

Correctness checks use deterministic fixtures across all EXIF orientations, gradients, patches and fine detail: coordinate round trips, crop coverage, CPU versus GPU differences and export interpretation, including centered Option resizing and angle sweeps that return without cumulative shrinkage. Color and export proofs choose explicit numerical tolerances and viewing conditions.

## Module activation

Logical boundaries and user enablement do not by themselves reduce memory or launch time. After the first external use case is selected, compare the built-in baseline with an activation prototype on the M4: minimal and default configurations, disabled and enabled-but-unused modules, first use, cold and warm launch, RSS, CPU/GPU allocations, idle work and first-use latency, with packaging size reported separately. Disabled modules must start no workers and allocate no processor resources. Reject any optimization that changes output or skips a recipe effect. If the benefit is immaterial, keep the simpler built-in implementation; the external-loader requirement remains regardless.

## Growth rules

Catalog opening never enumerates or decodes all originals. Grid memory depends on visible items and cache quotas. Import, hashing, thumbnailing and indexing use backpressure and resumable batches. Switching photos cancels obsolete preview requests, and the app stays interactive during exports and indexing. Introduce tiling with neighborhood halos before promising unrestricted large-image local processing. A cleanly reported resource limit is acceptable; silent exhaustion is not.
