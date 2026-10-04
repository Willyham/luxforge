# GPU-first rendering

Status: **decided by the owner on 2026-10-04; implementation planned** in [the plan](../../tasks/gpu-first.json). The release gate, `cargo xtask gpu-qualification`, is built, and its first run is [recorded below](#the-contract): the picture at 100% meets its limit, and at Fit, 33% and 50% the frame a drag draws does not, an [owner question](#proposals-with-recorded-defaults). The GPU becomes the renderer of record for the picture on screen, the histogram and clipping counts, point samples and export. Correctness is a declared tolerance against a CPU reference render, not bit identity. The CPU kernels stay as that reference and as the renderer for a machine without a usable GPU. The work converges in stages, each of which deletes a cache, a phase or a fallback, so the editor gets simpler as it gets faster.

## Why

The renderer is two renderers with an equivalence contract. The CPU f32 pipeline is the renderer of record; the GPU preview is an approximation qualified per program against a corpus, allowed during drags, with a fallback whenever it cannot promise its limit. Around the CPU path sit the mechanisms that make it fast enough to interact with: the Fit proxy, the restoration-prefix cache, the windowed proxy, the half-scale region, the settle phases and the quiet timer, the estimate store, the reduced-grid store, the spatial budget and the point-tile cache. Each has a key, an invalidation rule, a fallback and a test family, and the contracts between them (sample equals rendered byte, windowed equals whole, region equals full, settled Fit equals exact for Detail) are proofs over the workarounds.

| Part | Size on 2026-10-04 |
| --- | --- |
| CPU render pipeline, tiles, windows, point tiles, spatial budget (`crates/luxforge-core/src/render`) | 29,850 lines |
| Preview worker, proxies, phases, settles (`crates/luxforge-core/src/preview`) | 7,728 lines |
| GPU preview planner and programs (`crates/luxforge-core/src/render/gpu`) | 5,937 lines, 16 shaders |
| GPU fallback reasons | 11 |
| Window and region fallback reasons | 18 |
| Caches listed under rule 14 | 8 |

Every module already has a GPU program, every spatial unit included. What the GPU does not do today is the settled picture, the whole-image histogram, point samples, export and any zoom below 100%. The freeze of 2026-10-04 (a hover sample walking a 16 MP stage serially for four minutes) and the audit after it ([baseline](#the-baseline-this-replaces)) showed that the remaining cost of an ordinary edit is almost entirely the CPU path and the machinery around it, not the problem itself.

## Decision

Owner, 2026-10-04:

- The GPU is the renderer of record for display at every zoom, for the histogram and clipping counts, for `render.sample` and the other pixel reads, and for export.
- Image correctness is a **declared tolerance against the CPU reference render**, measured on the qualification corpus, for every output kind. Bit identity is not required of the GPU. On one machine with one driver the GPU is deterministic; across machines an export or a histogram may differ in the last digit, and that is accepted.
- The CPU kernels stay as the **reference renderer**: whole frames, no proxies, windows, point tiles or caches. It is the oracle every tolerance is measured against and the renderer a machine without a usable GPU falls back to, with a notice.
- The convergence is staged; no stage lands without the previous stage's tolerance evidence.

## The contract

**Renderer of record.** One GPU path renders the full stack at the size the view needs: the displayed size at Fit and at percentages below 100%, the visible window plus halos at 100% and above, and full-resolution tiles with halos for the histogram, samples and export. The RAW development stays a CPU step done once per source and white balance; its planes are uploaded as the GPU's source, with the mip chain the Fit view samples.

**Reference.** `luxforge_core::render` reduced to a whole-frame renderer: compile, one pass per segment, spatial operations over the whole stage in tiles for memory alone, no proxies, no windows, no point tiles, no caches beyond the prepared source. It renders slowly and plainly, and every test compares the GPU to it.

**Tolerance by output kind.** Measured on the corpus by `cargo xtask gpu-qualification` on 2026-10-04 for the kind the GPU renders, the picture, on the M4 Pro's Metal adapter with the harness built from `7d0c80be` ([performance](../specs/performance.md#gpu-qualification-against-the-reference)). The other kinds keep their proposed limits until the stage that renders them lands and the gate measures them. Figures are mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / \|signed mean ΔL\*\|, the largest over the cells named:

| Output | Reference | Limit | Measured |
| --- | --- | --- | --- |
| The picture on screen, at rest and in motion | The reference frame reduced to the view's size: the stack's exact whole frame, reduced by an area-weighted average of its linear light at Fit and below 100%, and its visible region, unreduced, at 100% | The existing [preview error limit](gpu-preview.md#the-preview-error-limit) of the recipe's class, pointwise 0.5 / 1.0 / 2.0 / 0.25 and spatial 1.0 / 2.5 / 5.0 / 0.5, now at rest as well as during a drag | At 100% the frame a drag draws is within it on all 250 cells (largest 0.084 / 0.514 / 0.948 / 0.049). At Fit, 33% and 50% it passes it on 475 of 744 cells, as the CPU proxy does, by up to 27.3 / 54.6 / 64.1 / 31.6; a process-first frame, the stack at full resolution reduced as the reference is, is within it on all 744 (largest 0.089 / 0.514 / 0.891 / 0.049). The limit stands and the gate fails; the question is the owner's ([below](#proposals-with-recorded-defaults)) |
| Histogram bins and clipping counts | The reference frame's counts | Each channel's summed absolute bin difference and each clipping count within 0.1% of the output pixel count | Not measured: the GPU renders no counts before stage 2. The gate computes the reference's and reports the kind as not rendered |
| A sample or pixel read | The byte on screen at that pixel | Equal by construction: the sample reads the rendered tile. Against the reference, within the display limit: the class's mean, p99 and signed mean ΔL\* over the samples (a 5 × 5 grid in the gate), a worst block having no meaning over scattered pixels | Not measured: the GPU answers no sample before stage 4 |
| Export | The reference export | The display limit per pixel: the class's four statistics over every pixel of the export; identical on one machine and driver; last-digit differences across machines accepted | Not measured: the GPU exports nothing before stage 4 |

**Where the numbers come from.** Each limit is measured against the reference on the corpus, per module and per stage, by `cargo xtask gpu-qualification` ([development](../engineering/development.md#image-correctness-exact-and-reference-buffers)), which `verify --tier rendered` runs after its scenarios as the release gate: a native run, not a preview qualification. A kind the GPU does not render yet is reported as such, neither a pass nor a failure; a cell that could not run, or a host without an adapter, makes the gate incomplete. A limit that cannot be met by a program is a defect of the program, never a reason to widen the limit without the owner.

**Headless and portability.** Native tolerance evidence comes from the M4 and, when available, Windows and Linux machines with a GPU. CI and VMs run the reference renderer's own tests and, on Linux, the functional journeys through a software Vulkan adapter; macOS has no software Metal, so a macOS VM proves the reference and the owner thread, not the GPU. The pillars' distinction between VM functional checks and native GPU evidence stands.

**What does not change.** The catalog owner thread, recipes, history, masks, the command registry and UI/API parity. The gesture's synchronous `draft.set` and the owner tasks on the blocking pool ([rule 12](../engineering/performance-rules.md#rules)). RAW development. The 2 GiB GPU preview budget and the photo-texture ceiling, whose accounting the rendering plan completes.

## Stages and what each deletes

| Stage | Work | Deletes |
| --- | --- | --- |
| 1 | The GPU draws every view: percentage zooms below 100% plan the preview at the displayed-size proxy as Fit does | `not-fit`; the CPU proxy per tick at 33% and 50% |
| 2 | The GPU renders the picture at rest (commits, history, presets, Compare) and the histogram and clipping counts come from a GPU reduction over the full stage in tiles; during motion the counts come from the displayed frame and are labelled updating | The CPU proxy and exact phases in the preview path, the quiet-settle whole-frame render, the restoration-prefix cache, Detail's settled-from-exact display, the settle dissolve's CPU side, the exact-phase progress bar's subject |
| 3 | Global estimates are computed on the GPU per frame: Dehaze's light and Clarity's reduced grid from a 1/16 reduced stage the GPU holds for the drafted prefix | The estimate store, the reduced-grid store, `EstimateAfterSpatial`, `region-estimate`, `window-estimate` |
| 4 | Samples read a pixel back from a GPU tile render; export renders full-resolution tiles on the GPU and encodes them; the reference export remains for machines without a GPU | The point worker and the point-tile cache, the deferred pixel-read rounds, the CPU export path's production use |
| 5 | Shader warm-up at launch and open; the software adapter on Linux CI; the reference renderer as the no-GPU fallback with a notice; the CPU production paths retired | Proxies, windows and regions, the spatial budget's render use, the settle phases; the architecture limits shrink to the GPU budgets and the prepared source |
| 6 | Qualification and measurement: the corpus comparison per output kind, native, and the latencies against the baseline below; the release gate | This plan |

## Constraints

- **Texture limits.** iced's wgpu limits cap a texture at 8192 px, and 60 MP of f32 RGBA is nearly a gigabyte per plane set. Full-resolution work on the GPU is tiled with halos, as the CPU does today; the 100% chain and its budget are that tiling and remain the only tiling.
- **Memory.** The GPU preview budget (2 GiB) and the photo-texture ceiling stay; the rendering plan's accounting of the resources outside them is a prerequisite of a total-memory claim, not of this plan.
- **Cold shader caches.** After an update a Presence drag's sequences took seconds to compile. With the GPU as the renderer of record the first picture would wait on them, so stage 5 warms the program set in the background at launch and open, and the fallback until then is the reference renderer, labelled.
- **Determinism.** Same machine, same driver: identical bytes frame to frame, which the rendered scenarios check. Across machines: last-digit differences accepted (owner).
- **Iced's one hop per frame** is unchanged; the surface primitive that writes its own texture is how the GPU frame reaches the screen.

## Acceptance

Each stage lands with: the tolerance comparison for every output kind it changes, on the corpus, native; the rendered scenarios whose frames it changes, with correlated state and events; the deletion of the mechanism the stage replaces, with its tests, docs, limits and rule entries; the quick tier. The plan's last task measures the latencies below against the baseline and records them with their scope, and the corpus comparison becomes the release gate the development guide names.

## The baseline this replaces

What an ordinary edit paid on 2026-10-04, from the read-only audit after the freeze. Figures marked *measured* are in [performance](../specs/performance.md); the rest are estimates from measured parts. This is the table the last task measures against.

| Cost | Trigger | Size | Stage |
| --- | --- | --- | --- |
| The estimate store holds 8 entries, oldest out, no refresh on a hit (`render/context.rs`) | Dehaze plus masked Dehaze layers evict the committed stack's lights within two renders | Cold reductions in point queries, *measured* 108 to 421 ms each, and GPU fallbacks at 100% and at a windowed Fit | 3 |
| A 120 ms pause during a drag starts a whole-frame exact render that a GPU tick never supersedes (`app/preview.rs` `quiet_refine`) | Any pause in a drag or stroke | About 1 s at 24 MP and 3 s at 60 MP on the pool while the drag goes on | 2 |
| The exact point path through nested spatial segments, serial, walking the stage on a miss (`render/spatial.rs` `PointTiles`) | `render.sample`, `mask.sample-input`, a colour-limited brush seed, the neutral picker | *Measured* 27 to 36 ms through Detail alone and 140 to 227 ms through one Presence segment; 125 to 231 s observed through three with a miss | 4 |
| No GPU preview below 100% (`not-fit`) | Every drag at 33% or 50% | The CPU proxy of the whole stage per tick, roughly 80 to 300 ms at 33% of 60 MP | 1 |
| `EstimateAfterSpatial` refuses the windowed path (`render/compiled.rs`) | Detail before Dehaze, or masked Dehaze after global Presence | 100% motion as a whole-output proxy, refinement as the whole frame, a crop at Fit as the whole proxy stage | 3 |
| A colour drag under Dehaze at 100% needs an exact-stage estimate per tick (`render/gpu/preview.rs` `RegionEstimate`) | Basic or a mask drag at 100% with Dehaze | *Measured* 110 to 420 ms per unit per tick, 864 ms for the first 60 MP region | 3 |
| Compare exit and history return re-render the exact frame | Every toggle | One exact render, about 1 to 3 s on a heavy stack | 2 |
| The whole-frame exact render and reduction after every commit at Fit | Every commit | The same second or three | 2 |
| RAW white-balance release held the window for its redevelopment | Every release | *Measured* 0.5 to 1.6 s before owner tasks left the update loop | Done |

## Proposals with recorded defaults

| Question | Default the plan runs on | Alternative |
| --- | --- | --- |
| The tolerance numbers | The table above: the picture's limit measured on 2026-10-04 and standing; the other kinds' limits as proposed until their stages are measured | Per-module limits |
| The picture at Fit, 33% and 50% against the reference: the frame a drag draws there, which processes a source reduced to the view's size as the CPU proxy does, passes the limits on 475 of 744 cells (2026-10-04, [performance](../specs/performance.md#gpu-qualification-against-the-reference)). Every source has cells past them, the Presence fixture none at Fit, where it is drawn at its own size. Pointwise, 203 of 312 cells, each past the worst block at least: the generated JPEGs (worst block up to 4.1), the Z6 and Air 2S (up to 2.2 / 9.4 / 10.9 / 0.54) and the zone plates under crops, warps and a luminance range (up to 12.0 / 29.7 / 52.1 / 9.1). Spatial, 272 of 432 cells, in every Presence and Detail family, the largest under Dehaze on the zone plates and the Presence fixture (up to 27.3 / 54.6 / 64.1 / 31.6) | The limits stand and the gate fails on these cells. Both candidates for the picture at rest are measured for the owner's choice: the frame a drag draws (reduce-first) and process-first (the stack at full resolution, reduced as the reference is), which is within every limit on all 744 cells | Hold the views below 100% to a reference rendered over a source reduced to the view's size, or to other limits; either is the owner's to set |
| A machine without a usable GPU | The reference renderer, whole frames, labelled in the status bar | Refuse to edit |
| Export device | The GPU where its programs are qualified; the reference renderer otherwise, and as an explicit "reference export" option for a test | GPU only |
| Counts during motion | From the displayed frame, labelled updating; exact counts from the full stage after each commit and release | Full-stage counts every tick |
| Owner tasks on the blocking pool, one frame per answer | Adopted 2026-10-04 | A hop-free path for microsecond requests, measured |
