# Roadmap

Outstanding work by area. What is delivered is in [feature status](features.md); pillars in [AGENTS.md](../AGENTS.md); accepted decisions in [decisions](decisions.md). Relative priority needs owner input ([open questions](decisions.md#open-product-questions)).

## Ready to implement

A persistent list of the work that can start now: each plan below has a validated design and task file on `main`, and each task named has status `ready`. Keep it current: add a plan when one of its tasks becomes ready, and remove a task when it starts or completes. The task files are organised by area in [tasks](../tasks/README.md). Product questions are resolved while planning, and ready tasks are concrete work with no further question or approval task; follow [planning for execution](../tasks/README.md#planning-for-execution). Working delivery comes before deferrable review, calibration and reference refinement.

**Minimum model: the high tier.** Every task in this section is implemented by Opus 5.5 High, Fable 5.1 High, 6-Astra High or 6.1-Sol High, or a stronger model. This applies to each agent that writes any part of a task, subagents included. A row may raise the minimum; none lowers it.

**Before starting a task**, re-review its plan, because `main` has moved since the plan was validated:

1. Read the design, then the task's description, context, links, acceptance and test strategy against current `main`. Confirm the files, symbols, commands, flags and figures it names still exist and still mean what it says.
2. Confirm every dependency is `completed` and no other agent has started the task: look for a branch or worktree carrying it (`git branch -a`, `git worktree list`) and continue that work rather than restarting it.
3. Check [decisions](decisions.md) and [feature status](features.md) for anything decided or delivered since that changes the task's scope.
4. If the plan no longer matches `main`, correct the plan and design first and run `cargo xtask check-repository`. Use the recorded delegation for routine implementation choices; resolve a newly consequential scope change during that focused plan update rather than inserting an owner-question task, while unaffected work continues. Then set the task to `in_progress` and remove it from this list.
5. Work and verify as [AGENTS.md](../AGENTS.md#how-we-work) says: narrow tests while building, the verification tier the task asks for at the end.

### Ready now

| Plan | Ready tasks | Notes | Minimum model |
| --- | --- | --- | --- |
| [Colour grading](../tasks/editing/colour-grading.json) ([design](design/colour-grading.md)) | TASK-002 initial grading unit; TASK-003 shared wheels and nested views | Product choices settled; working feature first, reference/Lightroom refinement last; no approval or research gates | High tier |
| [GPU memory accounting](../tasks/rendering/gpu-memory.json) | TASK-001 measure and bound GPU resources outside the photo-texture ceiling | A measurement: run after feature work, on a quiet host | High tier |
| [Corrections](../tasks/editing/corrections.json) ([design](design/corrections.md)) | TASK-001 freeze the remaining contract; TASK-002 Clone and Heal numerics | Repair before Detail, spatial GPU tolerance and exclusion from presets are decided; freeze the remaining contract and GPU-evaluable numerics | High tier |
| [AI editing](../tasks/editing/ai-editing.json) ([design](design/ai-editing.md)) | TASK-002 the prototype on `main` as the harness; TASK-006 the inference port and ONNX Runtime crate; TASK-008 the model-selection mask kind | The GPU-first integration they were sequenced after has merged; the Corrections foundation runs beside the first stage | High tier |
| [Dependency advisories](../tasks/project/dependency-advisories.json) | TASK-001 ttf-parser exception, due 2026-10-29; TASK-002 paste exception, due 2026-12-18 | The audit fails once an exception expires | High tier |

### Validated, awaiting authorization

Ready tasks in validated plans whose implementation the owner has not authorized. Some also have consequential choices to resolve in their design. The [product decisions](../tasks/project/product-decisions.json) plan carries the existing product questions. Do not start implementation without the owner's go-ahead.

| Plan | Ready tasks | Notes | Minimum model |
| --- | --- | --- | --- |
| [Lightroom import](../tasks/lightroom/lightroom-import.json) ([design](design/lightroom-import.md)) | TASK-001 confirm the catalog format against a copy of the owner's catalog | Needs no code; needs the owner's catalog copy | High tier |
| [Lightroom alignment](../tasks/lightroom/lightroom-alignment.json) ([design](design/lightroom-alignment.md)) | TASK-001 the rig's generator and synthetic targets | Its rounds need the owner to import and export in Lightroom | High tier |
| [HDR exposure merges](../tasks/library/hdr-merge.json) ([design](design/hdr-merge.md)) | TASK-001 durable chunked derived sources; TASK-002 RAW exposure resolution/fusion; TASK-003 alignment/deghosting | RAW-only scope and automatic alignment/deghosting are decided; independent foundations, working feature before final quality/timing; implementation awaits authorization | High tier |
| [Export settings](../tasks/rendering/export-settings.json) ([design](design/export-settings.md)) | TASK-001 settings, presets and API; TASK-002 JPEG options and limit search; TASK-003 WebP and TIFF encoders; TASK-004 resized and 16-bit output | Formats, direct-writing sheet, sharpening after the core and uncapped optimisation with a large-image warning and progress bar chosen; other defaults recorded in the design. Planning only; implementation awaits authorization | High tier |
| [High-zoom minimap](../tasks/interface/minimap.json) ([design](design/minimap.md)) | TASK-001 shared viewport projection; TASK-002 bounded overview rendering | Inclusive 200% threshold and click/drag navigation chosen; layout remains a proposed default. Planning only; implementation awaits authorization | High tier |

### In progress

[Auto tone](../tasks/editing/auto-tone.json) is implemented, with owner M4/corpus qualification outstanding: bounded sampling, the solver, API, desktop controls, presets and the local fitting rig. The owner's Lightroom exports and review come after delivery.

[Copy and paste settings](../tasks/interface/copy-settings.json): implemented with quick and native M2 / Metal verification; TASK-006 final rendered qualification remains in progress. Its project-wide GPU corpus gate needs the owner’s private RAW manifest, unavailable in this environment.

Continue these rather than starting them again: [GPU-first rendering](../tasks/rendering/gpu-first.json) TASK-008, portability without a native GPU: hosted Linux no-adapter checks and release acceptance pass; the lavapipe lane now retains small GPU-stage journeys and attempts every retained scenario before aggregate failure, with its passing hosted result still open. TASK-010, 011 and 012 record delivery and wait on it. [RAW looks](../tasks/raw/raw-looks.json) TASK-005 remains in progress: supplied-file journeys pass, but the rendered tier with the owner manifest and the Neutral Amount control remain outstanding. The [Efficiency](../tasks/rendering/efficiency.json) `dist` profile is blocked by the owner's deferral, and the [product decisions](../tasks/project/product-decisions.json) are the owner's to make.

## Engineering

**GPU previews** ([design](design/gpu-preview.md), [qualification](specs/performance.md#gpu-previews-qualified-on-the-m4)). Implemented and qualified on the M4; outstanding:
- The owner accepts the current error measure and picture limits, the 150 ms dissolve and status-bar presentation (2026-10-07); motion below 100% is reported, with the picture at rest strictly gated
- Native Windows and Linux correctness checks within the limits remain unrun. Hosted Linux passes the no-adapter fallback paths in prior runs; its lavapipe lane stops before histogram and `gpu-preview`, and supplies no native tolerance evidence
- Contention between a drag and the current GPU tile exports is unmeasured. The earlier 60 MP p95 18.7 ms and 24 MP miss past 32 ms were beside reference exports holding the shared CPU pool; `editor-latency --contend N` queues current exports for that check. The reduced-pool proposal in [instant previews](design/instant-preview.md#proposals-and-later-work) remains relevant to the reference path and unmeasured
- Behind Detail or an earlier Presence layer, a colour drag between that layer and Dehaze computes Dehaze's light every tick with those layers left out, the stand-in the owner kept for it on 2026-10-06 ([decisions](decisions.md#gpu-first-rendering)): five sharpen-stress cells under Dehaze −100 miss the limits in motion so ([performance](specs/performance.md#the-per-frame-lights-reduction-factor)). At rest, in export and for a sample the light is exact: from the staged sweeps' stage texture, or where that does not fit, from a light sweep that holds none and draws the layers before the light a second time ([design](design/gpu-preview.md#spatial-programs)). Where its stage texture does not fit, a light sweep draws those layers once more. After the throughput changes the headless surface indicates 1.63 to 1.65 s to the picture at rest against the same build's reference at 2.55 to 2.57 s; no 60 MP RAW is in the corpus for an editor timing ([performance](specs/performance.md#gpu-throughput-against-the-reference-task-012))
- On a cold Metal shader cache, as after an update, a Presence drag's sequences can take several seconds to compile after a Presence commit ([performance](specs/performance.md#presence-and-detail-sequences-in-the-editor)). Warm-up compiles the open stack's drags after the picture on screen's programs ([design](design/gpu-preview.md#warming-at-launch-and-open)); a drag before they are ready holds its frame for half a second, naming `compiling`, and then the reference renderer draws a whole frame per tick. Measured on three fresh bundles' empty Metal caches (2026-10-07): the GPU's first frame 0.79 to 0.84 s after the reference frame, the first bundle's warm-up 2.09 s and the next two 0.68 s, the system compiler cache outside the bundle serving them ([performance](specs/performance.md#gpu-throughput-against-the-reference-task-012)); a drag begun inside that window is not measured
- At 100%, Detail beside all three Presence fields charges up to 1.12 GB in the largest window the owner's display holds, each operation a link of the chain with planes of its own; a window of nearly twice its pixels, such as an external display's, would pass the 2 GiB budget, and its drag is drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging"), sharp at rest. The slot's shared scratch pool leaves this chain as it is: Detail's noise-reduction planes and Presence's are of other formats
- At 100% the window grows with every chained spatial layer, 207 px on every side for each masked Presence link of Texture and Clarity, and every link's intermediate and planes and the scratch pool take its size: the paint harness's layout fits 8 masks at 100% in its own view and 5 in the corpus's centred view, and past that a drag at 100% is drawn from its reduced stage scaled to the view, the status bar saying "Softer while dragging" ([performance](specs/performance.md#the-100-window-grows-with-the-chain)). **Proposals**, not built ([design](design/gpu-preview.md#later)): A, the GPU window planner grows a masked link's need only where its mask's bounds meet what its output must hold (about 1.44 GB at 10 masks in the harness's view, 2.38 GB at 16); B, each link's intermediate and kept planes sized to its own output's need (1.88 and 3.53 GB alone); both together 1.07 and 1.65 GB. A makes the boundary's key depend on the masks' bounds, so the drafted mask's link keeps its full halo, and needs a bit-exact test that coverage is zero outside a mask's bounds
- Painting over 10 to 16 masked Presence layers at Fit draws every tick on the GPU but misses the 16 ms p95 target (22.8 to 24.4 ms): a tick's GPU work is 8.8 ms over 10 masks and 14.5 ms over 16, past a 120 Hz frame ([performance](specs/performance.md#what-a-tick-costs-the-gpu))
- Proposal, not built: evaluate the ring of a 100% region that only Clarity's and Dehaze's 4× reductions read in strips, reducing as each strip is evaluated, so Detail's and Texture's full-resolution planes are held over the region alone, at the cost of a strip schedule and about 38% more Detail and Texture work in the ring ([design](design/gpu-preview.md#later))
- Later work in the [design](design/gpu-preview.md#later): region padding and Detail's sharpening change in one channel

**GPU-first rendering** ([design](design/gpu-first.md), [plan](../tasks/rendering/gpu-first.json)). Decided 2026-10-04 after a hover readout's point sample through Detail, Presence and masks froze the editor for about four minutes on a 16 MP RAW, and after an audit showed the remaining cost of an ordinary edit is the CPU renderer and the machinery around it. Done: the readout is removed, owner tasks run off the update loop (iced 0.14 had run them on it), every launch writes a bounded log, and the release gate, `cargo xtask gpu-qualification`, measures the corpus on the reference and on the GPU at Fit, 33%, 50% and 100%. Every zoom draws its drags on the GPU, and the GPU draws the picture at rest, every boundary derived from the photograph's source it holds: the gate passes by the recorded default, the picture at rest within its limits against the reference on every cell, while a drag's frame at Fit and below 100% is past them against the picture at rest and the reference alike on 462 of 783 cells, reported and not gated under the owner's accepted motion policy of 2026-10-07 ([performance](specs/performance.md#gpu-qualification-against-the-reference)). Also done: the histogram and clipping counts on the GPU, Dehaze's light per frame on the GPU, samples and export through GPU tiles, shader warm-up, the drags the CPU proxy and region drew drawn on the GPU, the reference renderer as the fallback for a machine without a GPU (a session without one dragging on the CPU proxy), and the CPU production paths retired, the CPU renderer reduced to the whole-frame reference. Correctness is a declared tolerance against that reference. Measured on the M4 on a quiet host against the 2026-10-04 baseline (2026-10-06, [performance](specs/performance.md#gpu-first-against-the-2026-10-04-baseline)): every drag measured draws each tick on the GPU within a display frame at Fit, 33% and 100%, under Dehaze at 60 MP and on a RAW white balance; a commit, a history return and Compare's exit put the stack on screen at once; a sample through three spatial segments answers in 92 ms at p95. After TASK-012 (2026-10-07, [performance](specs/performance.md#gpu-throughput-against-the-reference-task-012)) the picture at rest and its counts arrive sooner than the same build's reference renderer's frame and report on every measured stack, 60 MP and the masked stack included, and a heavy GPU export is 2.5 to 3.3 times faster than the reference export. Outstanding:
- A stroke over three masked Presence layers at 100% on the Air 2S, beside Detail and a global Presence, misses 16 ms at p95 (22.5 to 24.0 ms on 2026-10-07, 23.7 ms on 2026-10-08): its slow ticks are incremental, run the same links with no rebind and reach as far as its fast ones, and the coverage worker does not coincide with them; each is presented two frames late. The tick's GPU time against the frame, and the count pass, are not yet separated ([performance](specs/performance.md#painting-at-100-over-the-masked-stack))
- A trivial GPU export is slower than the reference export: 75 against 57 ms at 24 MP and 158 against 137 ms at 60 MP timed in the process. The gap is the stream's first band, its window upload and the device's first-tile wait; closing it needs a window uploaded beside the band before it or kept for the next, not built ([performance](specs/performance.md#the-trivial-exports-first-band))
- Launch to an empty shell (2.07 to 2.11 s) and the uncached 24 MP open (1.21 to 1.25 s) miss their provisional targets; measured beside the 2026-10-02 build on 2026-10-07, about 0.45 s of each since that day's record is the host's, on both builds, and this build adds 0.19 to 0.25 s to a launch, mostly the harness copying and macOS first-checking its larger executable
- A passing hosted result for Linux's retained lavapipe journeys, including histogram and `gpu-preview`
- The native Windows and Linux checks

**CPU and memory efficiency** ([design](design/efficiency.md), [plan](../tasks/rendering/efficiency.json)). Done and measured, except the `dist` build profile, which the owner deferred until the timing runs other plans have outstanding are recorded.

**GPU memory accounting** ([plan](../tasks/rendering/gpu-memory.json)). Measure and bound GPU resources outside the current 1 GiB full-photo ceiling and the separate 2 GiB GPU-preview and tile-worker budgets, including crop/overlay textures, native staging and pipeline/device overhead, before any total-memory guarantee. Attribution belongs in `resources.read` and the Performance panel (owner, 2026-10-07).

## Output

**JPEG export follow-ups.** JPEG export is delivered ([design](design/export.md)).
- **Export settings** ([design](design/export-settings.md), [tasks](../tasks/rendering/export-settings.json)) are planned:
  - an Export sheet with Web, Print and Master presets and the person's own
  - JPEG, lossy WebP and 8/16-bit TIFF, with resizing, optimised encoding, file-size limits and metadata levels
  - output sharpening as a second delivery

  The resized, 16-bit and sharpened tolerances are declared in the design. Settings, encoders and render paths can start independently before integration and the desktop. The request authorizes planning only.
- Wide-gamut output still needs its own decision and tolerance (owner, 2026-10-07)

## Library

**Browse, pick, develop** ([design](design/catalog.md)). The Select workspace, events and moments, picks, developing picks into the catalog, catalog folders and collections, Locate and resolving missing originals, removal and batch preset and export are implemented on the design's recorded defaults ([feature status](features.md)). What remains ([outstanding](design/catalog.md#outstanding)):
- Owner decisions on the design's proposals, P13's choice of how to meet the photographs view's time among them
- Recording quiet-host `catalog-measure` figures (the recorded loaded runs are not baselines), including the first browse from a card reader; meeting `browse.view`'s 50 ms p95 target over 100,000 photographs (107–188 ms measured); measuring a Develop drag beside a backlog of GPU-rendered developed tiers, beyond the camera-preview backlog workload
- A labelled corpus of real trips, bursts and brackets from several makes, to check events and moments against
- Native Linux and Windows runs of the folder and volume watchers
- In the desktop: dragging photographs onto a catalog folder, moving collections between groups, changing a smart collection's query, previews in Missing originals' rows, and Locate original… in export's refusal
- Background availability checks, and a browse filter for a missing value
- **HDR exposure merges** are [planned](design/hdr-merge.md): Import merged creates a saved catalog result and collapsed source stack; Pick merged also appends to the development set by default; originals stay individually pickable and removal/undo retains the result and edits. RAW-only inputs and automatic alignment/deghosting are decided; implementation is not authorized. Panorama stitching remains later and has no plan; the derived-source/merge-kind/stack boundary leaves room for it
- Catalog portability and backup, carrying each catalog's derived-artifact directory with it (decision pending)

## RAW

**RAW follow-ups** ([continuous RAW editing](design/initial-raw.md)). Continuous RAW editing is delivered; the owner closed its broader qualification plan on 2026-10-06 ([remaining boundaries](design/initial-raw.md#remaining-qualification-and-decisions)).
- Bayer highlight latitude: the owner decided to retain it, as X-Trans does, if the rendering change on the supplied Z6 and Air 2S files shows no new highlight artefacts; not built, so RCD still clips each gained Bayer site at sensor white
- Measure the cost and accuracy of a clip-aware white-balance draft on uncorrected Bayer developments, changing the GPU leading step and its CPU reference counterpart. Compare with the release through raw-panel, reporting drag differences and failing only gross errors; reconsider clamp semantics if Bayer highlight latitude changes ([decisions](decisions.md#gpu-first-rendering)). The candidate is in [instant previews](design/instant-preview.md#popular-cameras)

**RAW looks** ([design](design/raw-looks.md), [plan](../tasks/raw/raw-looks.json)). Decided 2026-10-05. New RAW photos start from a Luxforge look instead of the bare neutral development.
- Phase 1 (built; supplied-file look journeys pass; rendered tier with the owner manifest and disabling Amount on Neutral outstanding): the Standard look, chosen on the corpus and approved by the owner, in every new RAW photograph's Original; a Look section with Standard, Neutral and Amount; a Settings row for the starting look
- Phase 2: Match camera, a tone curve and chroma gain fitted per photo to its embedded camera preview off the owner, as a first-open entry or on request

**Camera coverage follow-ups** ([popular camera support](design/popular-camera-support.md)).
- Nikon High Efficiency NEF once upstream LibRaw decodes it; refused explicitly until then
- Sony A7 V compression and crop settings beyond the qualified lossless compressed (8845) and Compressed HQ (8846) samples, once a pinned decoder and authentic qualification cover them

## Editing tools

**Colour grading in the mixer** (decided, [design](design/colour-grading.md), [plan](../tasks/editing/colour-grading.json)). HSL / Grading tabs with three-way and individual/Global wheels, luminance, masks, Luxforge presets and direct Lightroom mappings. TASK-002 and TASK-003 can start now; TASK-009 hands off the working feature before TASK-010 independent-reference and Lightroom response refinement. Product choices are settled and initial numerical/layout choices belong to the implementer; no approval or research gate remains.

**Tone curve follow-ups** ([design](design/tone-curve.md)). The Tone curve is delivered ([feature status](features.md)).
- Owner review of the recorded defaults: what the composite acts on, channels, order, endpoints, the point limit, the Lightroom transfer and the editor gestures; below black the curve uses a floor-subtracted ratio (decided 2026-09-30)
- Follow-up: whether Basic's Blacks adopts the same floor-subtracted ratio, since lifting Blacks turns near-black noise into coloured speckle

**Corrections** (proposal, [design](design/corrections.md), [plan](../tasks/editing/corrections.json)). Remove blemishes by hand; offline Clone/Heal has its own delivery and qualification.
- Decided: repair after source development and before colour, deletion of `PointReplace`, stale patches kept rendering with export acknowledgement. Decided 2026-10-07: repair before Detail, the spatial GPU tolerance class and exclusion from presets, with Detail's downstream tick cost measured. Open: Heal numerics, brush/source-edge behaviour and bounded candidate/export shapes
- Reuse the delivered typed stroke store and drafts, module/capability contracts, Develop/prepare/adopt, Locate/library cleanup and nonlinear geometry; add the production brush interaction and ordered repair module
- Integrate on the GPU renderer of record: exact CPU reference fixtures, module-owned WGSL and required corpus families, tile-service reads/export, warm-up, and distant-source reads and changes declared to windows, staged sweeps and incremental chains; qualify offline UI/API, catalog previews, single/batch export and native performance after feature work
- Separately extend the same repair layer with frozen patches, bounded candidate jobs and source plus intra-layer prefix invalidation for AI editing; no model/provider prerequisite for the offline milestone

**AI editing** (decided 2026-10-05, [design](design/ai-editing.md), [plan](../tasks/editing/ai-editing.json)). Local, user-downloaded models behind one inference port and one analysis cache; nothing bundled, nothing sent without consent. Its harness and foundation tasks are ready on the merged GPU-first renderer, with the Corrections foundation beside its first stage.
- Decided 2026-10-07: reference inputs for all AI work; lock the generating photo while another remains editable; qualify permitted editing during inference and combined model/renderer memory, rechecking the 24 GB generative minimum.
- Decided: scope, quality-first model policy with no licence or provenance gate beyond a one-line use restriction, the runtime by quality then performance then maintainability, plain-language model choices, the effect region, stale patches kept rendering, budgets raised for quality, the 24 GB generative minimum, consent remembered per provider, the repair stage, sky replacement. Open: the first hosted provider, a fine-tune, a Swift shim, CPU-only machines
- Groundwork: the prototype on `main` as the harness; the fill-quality study (the 512 px fill's softness on large objects), the Select model qualification and the generative qualification (Moebius, FLUX.2 klein 4B) on the M4
- Foundation: the inference port and ONNX Runtime crate, `local-runtime` with activation and unload, the model manager with resumable multi-file downloads and a Models tab, the analysis task and cache, the hover worker and picker, the model-selection mask kind
- **Remove** on the Fast tier through the Corrections repair stage; **Select** with a class menu that follows the photograph (Subject, Background, Sky, People, Water, Mountains, Vegetation, Ground, Architecture, Objects)
- **Generative fill and Replace** on the local model, with variations, behind an experiment flag until qualified
- Optional: **sky replacement** as a deterministic layer over the Select Sky mask; people parts and depth-range masks
- Last: the typed remote-provider adapter and loopback protocol, Windows and Linux functional checks, qualification and measurement, documentation

**Presets follow-ups** ([design](design/presets.md#later)). The library, apply, create and Lightroom import are delivered.
- Owner review of the recorded defaults
- An Amount slider and a hover preview
- Calibrated RAW white balance preset import in the Lightroom alignment RAW white-balance round; the existing conversion matches white chromaticity, not a measured rendered response

**Lightroom import** (decided 2026-10-06, planned, not authorized; [design](design/lightroom-import.md), [plan](../tasks/lightroom/lightroom-import.json)). Bring a Lightroom Classic catalog or a folder of XMP sidecars across, read-only.
- Phase 1: the photographs worked on, catalog folders by event, collections, ratings and labels as collections, virtual copies and snapshots as versions, global settings, crop, orientation, lens and the look, the report and re-mapping, Lightroom's previews as first grid tiles only; no import undo, with the current catalog removal rule
- Phase 2: masks and local adjustments; phase 3: Camera Matching profiles, spot removal and AI masks as Match camera, Clone and Heal and Select land, and global and local Detail and manual perspective once alignment measures them

**Lightroom alignment** (decided 2026-10-06, planned, not authorized; [design](design/lightroom-alignment.md), [plan](../tasks/lightroom/lightroom-alignment.json)). Every supported slider follows Lightroom Classic's response, with targeted algorithm changes where a dimension still differs widely, in order of editing area: Basic, then Presence, the mixer and the vignette, then Detail, then masks and geometry, then RAW white balance.
- Open: authorization
- The rig (generated XMP, one Lightroom import and export per round by the owner), deterministic reference-rendered study buffers, round 1 of single settings and round 2 of pairs, calibrated mapping rows as the importers' bridge including RAW preset white balance in its round, a response change per setting, targeted behaviour changes with refrozen reference units and GPU programs qualified on the whole corpus, and validation on the owner's edited photographs

**Masks** ([design](design/masking.md)). Local adjustments. All four phases are delivered: the mask model and its persistence, the `mask.*` command family, the masked colour and spatial primitives, the Mask mode and panel, the coverage overlay — including for a mask that reads pixels, which [proposal P16](design/range-study.md#proposals) settled by reading the input of the mask's first bound layer once per display cell, measured and inside the preview budget, with the refusal kept where there is no operation to read or where reading one would cost a tile per cell — both gradients, brushes over the content-addressed stroke store, and the non-AI luminance and colour range selections with the colour-constrained brush.
- [Interaction repairs](design/masking-interactions.md): live candidate coverage, coherent selection and brush targets, unplaced/exclusive creation, `O` visibility and bounded live capture are implemented and verified. Native RAW liveness and scoped 24/60 MP hover/paint measurements pass; [performance](specs/performance.md#native-masking-interaction-qualification) records the memory and diagnostic limits
- [Mask performance](design/mask-performance.md) paints reference coverage overlays on their bounded worker. Photograph feedback uses the GPU; the coverage grid's contribution to slow ticks remains unseparated. The [matched native comparison](specs/performance.md#mask-feedback-and-the-coverage-handoff) measures the former CPU photograph path
- GPU painting over three masks holding masked exposure, Texture and Clarity is within a display frame at Fit (p95 8.5–9.0 ms in the recorded qualification). Misses remain over 10–16 masks at Fit and the Air 2S masked stack at 100% (p95 22.5–24.0 ms after the throughput changes); earlier CPU boundary-wait figures do not describe the current GPU-source path ([performance](specs/performance.md#painting-over-masked-spatial-layers))
- The range selections have no photographic corpus: every figure in the [range study](design/range-study.md) is over flat synthetic patches
- Density, edge-aware refinement, copying masks between photographs and mask presets are out of scope with their reasons recorded; model-based selections are the [AI editing](design/ai-editing.md) proposal's Select

**Auto tone** ([design](design/auto-tone.md), [tasks](../tasks/editing/auto-tone.json)). Implemented: an Auto button and `edit.auto-tone` that set Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation on the global Basic layer as one entry, from deterministic statistics and clipping searches over a bounded sample of the stage Basic receives, solved through Basic and the Look. Presets can carry Auto, recomputed per photo, and the Lightroom importer maps `AutoTone`. The local fitting rig is ready. The warm solve takes about 40 ms at 24 MP through Basic; through a Look above amount 100 it takes about 0.3 s and misses the provisional budget, and click-to-entry is not yet measured. The owner M4/corpus qualification and fit to Lightroom's Auto on the owner's photos remain open. Implementation authorized on 2026-10-08.

**Tuning delivered tools.** Refine the recorded defaults of Presence, the colour mixer and the vignette (decision pending).

## Interface

**Copy and paste settings** ([design](design/copy-settings.md), [tasks](../tasks/interface/copy-settings.json)). Implemented with quick and native M2 / Metal verification; final corpus qualification remains open. Copy chosen adjustment groups from one photograph into a per-window clipboard and paste them onto the open photograph, a Develop filmstrip selection (new multi-selection) or a Select selection, or from the previous photograph, one history entry per photograph through `edit.apply-settings` and `batch.apply-settings` with a `paste` origin. Geometry, masks, Sync and a system clipboard document remain later work.

**High-zoom minimap** ([design](design/minimap.md), [tasks](../tasks/interface/minimap.json)). Planned for Develop at percentage zoom ≥200: whole-image overview, visible-region rectangle and owner-chosen click/drag navigation through the existing `view.set` path. Shared geometry and bounded overview rendering can proceed independently before UI integration; final native qualification and photo-sized measurements follow working delivery. The request authorizes planning only.

**UI themes** ([design](design/ui-themes.md)) are implemented, with Omarchy import and six bundled Omarchy themes ([feature status](features.md)). Outstanding:
- Owner review of the recorded defaults: which themes are bundled, the contrast floors, the Omarchy forms read, the canvas background's Theme default and the Appearance tab
- Later, if the owner wants them: following Omarchy's current theme on Linux, following the system's light or dark appearance

## Programmability

**Live-session CLI** ([design](design/live-cli.md)) is implemented: `luxforge-ctl` drives the catalog the desktop has open through its live session ([feature status](features.md), [user guide](user-guide.md#the-live-session-command-line)). Operation-specific aliases wait until real use shows a need for one.

**MCP adapter.** Expose the whole operation registry to agents through a standards-compliant MCP server over the existing command service.

**Shared editing** (future, [design](design/shared-editing.md)). One host, one invited collaborator or agent, one photograph. No milestone.

## Extensibility

**Shared module capabilities follow-ups** ([design](design/module-capabilities.md)). Settings and secrets, consent, the transport, resources, tasks and derived artifacts are delivered on macOS; activation is deferred until a module needs `local-runtime`.
- Owner review of the recorded defaults: who may grant, loopback-only plain HTTP ([decisions](decisions.md#module-capabilities)); per-asset photo consent was revised on 2026-10-05 to consent remembered per provider, landing with AI editing's remote tier
- Windows Credential Manager and Linux Secret Service for module secrets, verified natively; both refuse with `not-ready` today
- Native Windows and Linux checks of the transport's certificate verification and of resource removal, which on Windows must release a module's files before deleting them
- The first reviewed provider adapters and their crop and mask data classes, with [AI editing](design/ai-editing.md)
- Resumable, hash-checked downloads for large model files; an interrupted download restarts today
- Setting and clearing secrets off the catalog owner, so an OS keychain prompt never holds other clients
- `managed-storage` when a module first needs it; `local-runtime` is defined by the [AI editing](design/ai-editing.md) proposal as its first consumer

**External modules.** Load separately authored modules.
- Measure optional-module activation cost, including shader compilation and warm-up on both GPU devices, disabled and enabled-but-unused configurations, first use, launch, RSS, CPU/GPU allocations and idle work
- Choose the first use case (decision pending)
- A processing module supplies an exact-fixture CPU reference unit and module-owned WGSL, qualified within the declared GPU tolerance on corpus stacks; spatial modules carry windows, halos, staged sweeps and incremental change propagation
- Loader proof with a real process or ABI trust boundary, preserving common tile reads, GPU export with reference fallback and the gpu-free core

## Inspection

**Performance panel follow-ups** ([design](design/performance-panel.md)). The Performance section, the activity board and the resource counters are delivered.
- GPU time and allocations on Linux (DRM `fdinfo`) and Windows (D3DKMT), and native checks of the CPU and memory counters there
- Attribute memory in `resources.read` and the Performance panel (owner, 2026-10-07; not implemented). The desktop supplies its surface and tile-worker charges to the core: source, chain, intermediates, scratch, rest tiles, photo slots and retiring resources. Native staging, device/pipeline overhead and unified-memory overlap remain under [GPU memory accounting](../tasks/rendering/gpu-memory.json); application ceilings are not process bounds
- Reduce the open section's whole-window redraw cost: idle model updates and unchanged GPU writes are cheaper, but an aggregate idle CPU improvement is not established
- A rendered frame of a RAW development while it runs

## Platform and release

**Full-editor verification.** Native M4 handoff of the complete editor, then Windows and Linux.

**Cross-platform builds.**
- macOS hosted build, reference checks, editor acceptance and packaging pass. Linux no-adapter checks, release acceptance and packaging pass. The lavapipe lane uses small GPU-stage fixtures and runs every retained scenario before aggregate failure; its passing hosted result remains open. Native Windows/Linux GPU and desktop checks remain separate acceptance; Windows CI is disabled
- Windows and Linux packaging, checked in real desktop sessions
- Reproducible Linux VM route
- Developer guide checked on Windows and Linux

**Dependencies.**
- Remove or re-review the ttf-parser (by 2026-10-29) and paste (by 2026-12-18) advisory exceptions ([plan](../tasks/project/dependency-advisories.json))
- Automated license, asset and advisory checks; the manual review stays deferred

## Not in scope

Map, Book, Slideshow, Print, Web and Publish Services. Accounts, cloud sync, built-in AI chat, a plugin marketplace, a public compatibility framework and a generalized processing graph. General bitmap layers with blend modes. AI denoise, upscaling, generative expand, text-to-image and face retouching are not proposed ([AI editing](design/ai-editing.md) covers removal, selection, generative fill and sky replacement).
