# GPU previews

Status: **planned; not implemented.** The owner set the direction on 2026-10-01: during a gesture, speed comes first, as long as the picture does not visibly jump when it settles. The error limits, the dissolve, the label and the preference below are proposals with recorded defaults. The plan runs on them until the owner revises them. [Task plan](../../tasks/gpu-preview.json) · [decisions](../decisions.md#gpu-previews).

## Outcome and scope

While a person drags a slider, moves or paints a mask, or drags at 100%, the desktop evaluates the part of the recipe the gesture changes on the GPU. It draws the result in the same display frame as the input. When the gesture settles, the CPU's result replaces it through a short dissolve.

The CPU renderer stays the only reference. Settled frames, the histogram, clipping counts, point samples, mask coverage grids, analysis, export, history and every API answer are computed exactly as today and never read a GPU pixel.

### Why

| Cost today | Figure | What the GPU stage changes |
| --- | --- | --- |
| Fit drag with a full Basic layer, input to presented frame | 18.1 / 24.8 ms p50 / p95 at 24 MP, 17.2 / 28.9 at 60 MP; the core proxy render with geometry is about 45 ms ([performance](../specs/performance.md#instant-previews-proxy-phase-hop-rule-and-the-surface-primitive)) | A tick becomes a uniform write and a few passes in the surface's own `prepare`, with no worker hop and no per-tick upload |
| A Fit proxy while an exact render holds the shared pool | 12.1 / 13.1 ms alone, 207.2 / 214.7 ms contended ([performance](../specs/performance.md#raw-colour-row-batching)) | The GPU stage does not share the Rayon pool, so settlement and RAW development no longer slow the gesture |
| A painted stroke | p95 42.1–50.2 ms over four masked layers, missing the provisional bound. On a bare masked layer the worker render is only 4.5–5.4 ms; the queue wait and result delivery dominate ([further performance](../research/further-performance.md)) | The hop that dominates is removed, not just the kernel |
| 100% motion | A half-scale region, softer until refinement | A full-scale region at motion speed |
| Presence and Detail | All three Presence fields 1.5–1.6 s exact at 60 MP; a Presence drag at Fit 16.5–22.7 ms p50; Detail drags are expected to miss the Fit budget ([Detail](detail.md#proposals-with-recorded-defaults)) | Their drags, and drags of layers before them, run on the GPU |

The recorded verdict that a GPU colour stage was not justified ([further performance](../research/further-performance.md#rgba-ownership-gpu-and-simd-choices)) weighed the kernel alone. This design removes the worker hop, the per-tick upload and contention with exact work, which that verdict did not weigh.

### In scope

1. A preview-difference measure and corpus, so every limit is measurable before any GPU code exists.
2. The GPU stage in the desktop: device sharing, a bounded preview slot, fallback and evidence.
3. GPU programs for the seven pointwise colour units (Exposure, White balance, Tone, ColourAdjust, Tone curve, Mixer, Vignette) and every mask kind (linear, radial, brush, luminance range, colour range). Each program is owned by its module, beside its CPU unit.
4. Fit drags over a held input boundary, then 100% drags over a held region.
5. The settle hand-off: the dissolve, the label, motion overlays and a preference with an API equivalent.
6. Presence (Texture, Clarity, Dehaze), then Detail (sharpen, denoise), as GPU preview programs.
7. Mipmapped minification for a full-resolution texture drawn below its size. This is a display fix that needs no error limit.
8. Native qualification and measurement.

### Out of scope

- The GPU for exact renders, export, the histogram, clipping counts, point samples, mask coverage grids, RAW development, JPEG decoding or encoding, or proxy builds.
- A GPU dependency in `luxforge-core` or in the headless `luxforge-json` binary.
- View changes without a draft: a pan or zoom with nothing being dragged keeps the CPU path ([later](#later)).
- Crop and transform drafts, whose input stage the surface already turns on the GPU.
- A general GPU node graph and external GPU modules.

## Decisions

Decided by the owner on 2026-10-01:

- **Speed comes first for interactive previews.** The constraint is that settling must not produce a large, noticeable jump in the image.
- **GPU arithmetic is for previews only.** The CPU stays the reference for settled frames, analysis, sampling and export. Apple GPUs have no 64-bit floating point, so the CPU's `f64` arithmetic, bit-identical mask coverage and byte-identical sample/render contract cannot be reproduced on them. Making the GPU the reference is not pursued.
- **Order of work.** Error limits first, then the Fit colour stage, then Presence and Detail, with mipmapped minification beside them.

### Proposals with recorded defaults

The plan runs on these; each is a proposal the owner can revise.

| Question | Default | Alternative |
| --- | --- | --- |
| Error measure | CIEDE2000 between the GPU frame and the CPU frame that replaces it, over the photograph only ([below](#the-preview-error-limit)) | 8-bit code differences, which overweight highlights and underweight shadows |
| Error limits | Two classes, in the [table below](#the-preview-error-limit) | Tighter limits that rule out half-precision storage and fast transcendentals |
| Settle transition | A 150 ms linear-light dissolve from the GPU frame to the CPU frame | A hard swap, as today |
| Label | The status bar's render slot reads "GPU preview · N ms", beside today's "Approximate render · N ms" and "Exact render · N ms". Nothing is drawn on the photograph | A badge on the canvas |
| Preference | On by default; `workspace.set {gpu_preview}` and a command-palette entry | No preference: automatic fallback only |
| Storage precision | `rgba16float` for the boundary and colour intermediates; `r32float` for spatial accumulators | `rgba32float` throughout, at twice the memory |
| GPU preview budget | 256 MiB for every GPU-preview texture and buffer, beside the photo-texture ceiling; a program that does not fit takes the CPU path | No separate budget |

## The preview error limit

**What is compared.** The jump a person sees: the GPU frame on screen at the moment of settlement against the CPU frame that replaces it. Both are taken at the same view and as the 8-bit sRGB pixels the surface draws.

- At Fit, the replacing frame is the CPU proxy render, or for a stack whose effect declares `FitSettle::Exact` (Detail), the reduction of the exact render.
- At 100% it is the exact visible region.
- Statistics cover the photograph alone, not the canvas beside it, as the owner decided for the white-balance gates on 2026-09-30.

**The measure.** Per-pixel CIEDE2000 (ΔE00), from sRGB through CIELAB under D65, computed in `f64` by an independent reference. As a common guide, ΔE00 below 1 is not perceptible, 1 to 2 is perceptible on close inspection and 2 to 10 at a glance. Four statistics:

- **Mean ΔE00** over the photograph: the overall shift.
- **Worst block**: the largest mean ΔE00 of any 16 × 16-pixel block. A region that changes is what reads as a jump, more than scattered pixels do.
- **p99 ΔE00**: edges and fine detail.
- **Signed mean ΔL\***: whole-picture brightening or darkening, which reads as a jump even when small.

**Limits** (proposed):

| Statistic | Pointwise programs: colour units, masks, vignette | Spatial programs: Presence, Detail |
| --- | --- | --- |
| Mean ΔE00 | ≤ 0.5 | ≤ 1.0 |
| Worst 16 × 16 block mean ΔE00 | ≤ 1.0 | ≤ 2.5 |
| p99 ΔE00 | ≤ 2.0 | ≤ 5.0 |
| Signed mean ΔL\* | within ±0.25 | within ±0.5 |

The pointwise mean matches the precedent the owner already set: the RAW white-balance draft gate is a mean within one code at Fit. The spatial limits are deliberately generous. They leave room for half-precision storage, single-precision running sums and estimates taken at the scale the GPU holds. Under a 150 ms dissolve, a regional difference of about 2 reads as the picture settling rather than popping. The first task measures today's accepted CPU approximations under the same measure, for comparison:

- the Presence proxy against the exact downscale
- the half-scale 100% motion region against the exact region
- a RAW white-balance draft against its release

These limits gate **enabling** a program, not frames at run time. A program, or a stack class, that misses any limit on the corpus ships disabled, and those stacks take the CPU path until it passes.

**Hard rules, not limits.**

- A finite CPU value is never non-finite on the GPU.
- No tile or region seams.
- A mask edge's half-coverage contour lies within a quarter of a display pixel of the CPU's.
- A GPU frame never feeds a histogram, clipping count, sample, mask grid, export, artifact or history entry.
- Settlement always replaces the GPU frame with the CPU's.

## Design

### Where the code lives

- **Programs belong to modules.** Each module's GPU program is WGSL source text kept beside its CPU unit in `luxforge-core`, with no wgpu dependency there.
  - `PointwiseColor` and the mask components gain an optional description: the WGSL function, a uniform block of 32-bit words and, where needed, a storage block (the curve's knots, a brush's segment grid). A unit with no description takes the CPU path.
  - Two units that `describe()` identically produce identical uniforms.
  - `cargo xtask check-repository` keeps refusing a GPU crate in the core's and `luxforge-cli`'s normal dependencies. The WGSL is validated in core tests through `naga`, as a dev-dependency only.
- **Planning stays in the core.** The compiled evaluation answers, in `O(layers)` on the catalog owner, the ordered GPU plan from a boundary to the terminal output, or the reason there is none. The plan holds the programs, mask blends, the geometry mapping (an affine matrix, or a coarse coordinate grid for a lens or perspective warp) and the finish units.
  - Possible reasons for no plan: a pixel-stage layer, a unit without a program, a disabled program, or a spatial unit before its program exists.
- **Execution lives in the photo surface.** The surface already receives Iced's wgpu device and queue in `PhotoPrimitive::prepare` (`crates/luxforge-ui/src/photo_surface.rs`). It takes the plan as plain data (WGSL text, uniform bytes, textures), so `luxforge-ui` still never names a core type. It assembles and caches one pipeline per program sequence and encodes the passes in the frame that draws them.
  - The UI thread only encodes GPU commands; it never waits for the GPU or reads pixels back.
  - The desktop converts the core's plan into the surface's plain data in `luxforge-app`.
- **How the plan reaches the surface** is settled in the foundation task. The plan is preview state like the proxy, not a history or API result. The desktop receives it with the draft's synchronous answer, so a tick adds no hop (performance rule 12). Whether it travels in the desktop's typed owner reply or another in-process channel is an implementation choice.

### The held input boundary

A GPU tick evaluates only what the draft changes. When a draft begins, the preview worker renders the **boundary**: the input of the earliest layer the draft changes, in content space, at proxy scale at Fit or over the visible region plus the operation's summed halo at 100%.

- **Contents.** The prefix's colour, restoration and spatial work is baked into the boundary once. A Basic drag on a JPEG with no earlier layer uses the proxy source itself. On RAW the colour prefix is pulled by rows today, so the boundary is a new frame, built through the same windowed planner.
- **Lifetime.** The boundary is uploaded once, as `rgba16float` linear sRGB, and held for the open draft only. It is released when the draft ends or the boundary's key changes. The key holds the source, development, prefix identity, scale and window.
- **Until it is ready.** Ticks take the CPU path exactly as today, so the first frame of a gesture is never later than now.

This is the [held-boundary proposal](instant-preview.md#proposals-and-later-work) applied to the GPU: the CPU builds the boundary once per draft, and the GPU evaluates everything after it.

### A tick

1. The input updates the draft on the owner synchronously, as today. The answer carries the new GPU plan's uniforms.
2. Nothing goes to the preview worker for that tick: no proxy job and no upload.
3. In the next `prepare`, the surface writes the uniforms and encodes the passes:
   - the colour and mask programs in content space, in recipe order;
   - the geometry tail, by bilinear sampling through the affine matrix or the coordinate grid (linear-light texels, as the CPU's resample blends);
   - the finish programs (the vignette) in output space;
   - the clipping marks, when clipping is shown;
   - encoding to the surface format.
4. The frame draws in that same update.

Colour runs before geometry, as on the CPU, so a strong curve across an edge is not interpolated in the wrong order. A lens or perspective warp's coordinate grid is computed once per draft from `GeometryMap`, at a density that keeps the mapping within 0.1 display pixel of exact.

### Settle and the dissolve

The shared quiet policy and release start CPU settlement as today: the proxy, or the exact visible region, and then the exact phase. When the CPU frame for the GPU frame's content arrives, the surface dissolves from one to the other in linear light over 150 ms.

- Redraws are requested only while a dissolve runs, so an idle editor stays asleep (performance rule 8).
- An input during a dissolve cancels it and draws the next GPU frame.
- The evidence snapshot records the dissolve's from and to identities and its progress.

### Labels and overlays during motion

- **Status.** While a GPU frame is shown, the status bar's render slot reads "GPU preview · N ms", timed from GPU timestamps where the adapter has them. The histogram keeps its existing updating state.
- **Clipping.** The GPU program marks clipped output pixels itself, so clipping follows a GPU drag. It is labelled approximate, as the proxy-derived overlay is today, and whole-image counts stay exact CPU work.
- **Mask tint.** During a GPU mask drag, the tint is drawn from the coverage the program evaluates. The coverage worker's grid stays authoritative and replaces it on settle.

### The preference

`workspace.set {gpu_preview: bool}` holds the per-client preference, reported in session state, on by default. A command-palette entry toggles it. With it off, every gesture takes the CPU path.

The session state and the evidence snapshot report which path drew each frame (`gpu` or `cpu`), and why a GPU-eligible gesture fell back:

- the preference is off
- no adapter
- the device was lost
- a shader failed to compile
- the budget would be exceeded
- a unit has no program, or its program is disabled
- a pixel-stage layer is in the stack

The desktop's own client sets the preference; an agent reads it through the same method.

### Fallback and portability

- Losing the device mid-gesture moves the next tick to the CPU path with its reason; nothing waits for recovery.
- Every gesture works with no GPU path at all.
- Windows and Linux get functional checks on whatever adapter they have, including a software adapter. These are not native performance evidence. Native GPU evidence is the owner's M4.

### Bounds

- **One budget.** Every GPU-preview texture and buffer is charged to one 256 MiB budget, beside the photo-texture ceiling and reported with it: the boundary, intermediates, spatial accumulators, coordinate grids and storage blocks.
- **Typical sizes.**
  - A Fit boundary is at most 8 MP at 8 bytes per pixel, so at most 64 MiB.
  - A 100% boundary is the visible region plus its halo.
  - A spatial program's intermediate planes are its largest consumer.
- **When it does not fit.** A plan that would not fit takes the CPU path and names the budget. The GPU-preview budget is not part of the 1088 MiB photo-texture engineering ceiling. Like the crop stage's textures, it is a GPU resource the standing GPU memory accounting work must count before any total-memory claim.

### Spatial programs

Presence and Detail become compute programs over the boundary. They use the CPU's operations, not its exact arithmetic:

- **Presence:** separable box means and minima, self-guided and guided filters, the 4× block reduction and bilinear upsample, and the soft clip.
- **Detail:** separable Gaussians, the à-trous wavelet levels, coring, gradient-energy gating, 3 × 3 extrema and the limiter.

Single-precision running sums are reseeded at intervals, or replaced by a two-level prefix sum, whichever passes the limits more cheaply. The GPU holds a whole proxy stage or region, so it has no tile halo to recompute.

A global estimate (Dehaze's atmospheric light) comes from the estimate store when its key matches the boundary's content. Otherwise the GPU computes it from the stage it holds, and the frame is labelled approximate, as the CPU proxy already is.

### Mipmapped minification

Before/After keeps the exact raster it displays as its After side, even at Fit ([before and after](before-after.md#after-at-fit)). A full-resolution texture drawn at that size through the bilinear sampler with no mip levels aliased: on the zone plate it showed full-contrast replica rings and read 15 codes too dark beyond the display's Nyquist limit, the same failure the crop draft's input stage showed ([instant previews](instant-preview.md#a-crop-drafts-input-stage); [performance](../specs/performance.md#a-minified-after-side-at-fit)).

The photo surface now gives a full-resolution photograph drawn under half its size on both axes a chain of mip levels, and otherwise leaves its texture as it was. No arithmetic in the recipe changes, so no error limit applies.

- **Linear light.** Each level is the mean of the 2 × 2 texels above it, written by a render pass that reads the level above through an sRGB-typed view of the texture and writes the next level through the same, so the mean is taken in linear light whether the pipeline's textures are sRGB-typed or not. The sampler's trilinear filter reads between the two levels that bracket the drawn scale.
- **GPU only, never waited on.** The passes are encoded and submitted from `prepare` on the UI thread, once for each new frame written while it is drawn that small, with no readback and no wait.
- **Accounting.** The levels belong to the photo slot that holds them: a chain is a third more than its base, counted in the slot's bytes, the shared photo-slot budget and the retirement of the slot, and reported as `mip_resident_bytes` and `mip_generations`. A texture that has a chain keeps it until it is rebuilt; a texture drawn at its size has none. A chain's texture is cut to the frame's exact size, which for a 24 MP frame is smaller than the bucket a plain texture reserves.
- **When the chain does not fit.** The chain needs the photograph in one texture and its whole size within one full-allocation cap. A photograph the device holds in tiles (wider or taller than 8192 pixels) has none, because each tile's filtering apron would put a seam into every level. Before/After then draws the exact-derived display reduction it holds of that photograph at Fit instead, so a 60 MP After has no full-size texture while compared at Fit.

## What is preserved

- Originals, recipes, history, drafts, commits and every mutation are untouched. The CPU proxy, exact phase, analysis, sampling, overlays and export produce the same bytes as before, which the existing exact tests and fixture hashes prove.
- The owner thread plans in `O(layers)` and touches no pixel. The UI thread encodes GPU commands and never renders on the CPU or waits on the GPU.
- Every GPU-preview allocation is bounded and released with its draft. The preview queue's one active and one pending job, its cancellation and the shared quiet policy are unchanged.

## Verification

- **Reference.** An `f64` CIEDE2000 and the four statistics in `luxforge-reference`, checked against the published CIEDE2000 test pairs.
- **Per program.** A headless readback test against the CPU unit over dense synthetic inputs, including negative and over-range linear values, judged by the limits and the hard rules. A test without an adapter reports that it was skipped, never a pass.
- **Corpus.**
  - Sources: the generated 24 MP and 60 MP JPEGs, the zone plate, the Presence fixture and the Z6, X100VI and Air 2S RAWs.
  - Recipes: full Basic, the Tone curve, the Mixer, the Vignette, every mask kind, a straightened crop, a lens and perspective warp, Presence combinations and Detail.
  - Both Fit and 100%, each program judged against its class's limits.
- **Native.** Rendered scenarios with correlated state and logs: path, reason, dissolve and preference, with readbacks of GPU and settled frames.
- **Timing.** Fit drags at 24 MP and 60 MP with a full Basic layer, against the 16 ms target; the same drag while an exact render holds the pool; brush paint latency; 100% drags; Presence drags and a Basic drag under Presence on the X100VI. Also idle after a dissolve and GPU-preview memory high-water.

## Later

- View changes without a draft (a pan at 100% or a zoom between Fit and 100%) over a source-region boundary, which would also shorten a cold Dehaze first region (863.91 ms at 60 MP).
- A RAW white-balance draft on the GPU: the approximation `W` is a 3 × 3 matrix over the developed planes and fits the pointwise class.
