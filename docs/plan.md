# Roadmap

Outstanding work by area. What is delivered is in [feature status](features.md); pillars in [AGENTS.md](../AGENTS.md); accepted decisions in [decisions](decisions.md). Relative priority needs owner input ([open questions](decisions.md#open-product-questions)).

## Ready to implement

A persistent list of the work that can start now: each plan below has a validated design and task file on `main`, and each task named has status `ready`. Keep it current: add a plan when one of its tasks becomes ready, and remove a task when it starts or completes. The task files are organised by area in [tasks](../tasks/README.md).

**Minimum model: the high tier.** Every task in this section is implemented by Opus 5.5 High, Fable 5.1 High, 6-Astra High or 6.1-Sol High, or a stronger model. This applies to each agent that writes any part of a task, subagents included. A row may raise the minimum; none lowers it.

**Before starting a task**, re-review its plan, because `main` has moved since the plan was validated:

1. Read the design, then the task's description, context, links, acceptance and test strategy against current `main`. Confirm the files, symbols, commands, flags and figures it names still exist and still mean what it says.
2. Confirm every dependency is `completed` and no other agent has started the task: look for a branch or worktree carrying it (`git branch -a`, `git worktree list`) and continue that work rather than restarting it.
3. Check [decisions](decisions.md) and [feature status](features.md) for anything decided or delivered since that changes the task's scope.
4. If the plan no longer matches `main`, correct the plan and design first and run `cargo xtask check-repository`; take a consequential product change to the owner rather than deciding it. Then set the task to `in_progress` and remove it from this list.
5. Work and verify as [AGENTS.md](../AGENTS.md#how-we-work) says: narrow tests while building, the verification tier the task asks for at the end.

### Ready now

| Plan | Ready tasks | Notes | Minimum model |
| --- | --- | --- | --- |
| [GPU memory accounting](../tasks/rendering/gpu-memory.json) | TASK-001 measure and bound GPU resources outside the photo-texture ceiling | A measurement: run after feature work, on a quiet host | High tier |
| [RAW looks](../tasks/raw/raw-looks.json) ([design](design/raw-looks.md)) | TASK-006 freeze the camera fit (phase 2) | Phase 1 is built; its TASK-005 waits on the native `look` scenario and the rendered tier | High tier |
| [Corrections](../tasks/editing/corrections.json) ([design](design/corrections.md)) | TASK-001 freeze the remaining contract; TASK-002 Clone and Heal numerics | Renderer integration targets the merged GPU-first interfaces; the repair-versus-Detail placement is still open | High tier |
| [AI editing](../tasks/editing/ai-editing.json) ([design](design/ai-editing.md)) | TASK-002 the prototype on `main` as the harness; TASK-006 the inference port and ONNX Runtime crate; TASK-008 the model-selection mask kind | The GPU-first integration they were sequenced after has merged; the Corrections foundation runs beside the first stage | High tier |
| [Dependency advisories](../tasks/project/dependency-advisories.json) | TASK-001 ttf-parser exception, due 2026-10-29; TASK-002 paste exception, due 2026-12-18 | The audit fails once an exception expires | High tier |

### Validated, awaiting authorization

Ready tasks in plans the owner has decided but not yet authorized for implementation (the [product decisions](../tasks/project/product-decisions.json) plan carries the question). Do not start them without the owner's go-ahead.

| Plan | Ready tasks | Notes | Minimum model |
| --- | --- | --- | --- |
| [Lightroom import](../tasks/lightroom/lightroom-import.json) ([design](design/lightroom-import.md)) | TASK-001 confirm the catalog format against a copy of the owner's catalog | Needs no code; needs the owner's catalog copy | High tier |
| [Lightroom alignment](../tasks/lightroom/lightroom-alignment.json) ([design](design/lightroom-alignment.md)) | TASK-001 the rig's generator and synthetic targets | Its rounds need the owner to import and export in Lightroom | High tier |

### In progress

Continue these rather than starting them again: [GPU-first rendering](../tasks/rendering/gpu-first.json) TASK-008, portability without a native GPU, which TASK-010 and 011 wait on. The [Efficiency](../tasks/rendering/efficiency.json) `dist` profile is blocked by the owner's deferral, and the [product decisions](../tasks/project/product-decisions.json) are the owner's to make.

## Engineering

**GPU previews** ([design](design/gpu-preview.md), [qualification](specs/performance.md#gpu-previews-qualified-on-the-m4)). Implemented and qualified on the M4; outstanding:
- Owner review of the proposed error limits, the 150 ms dissolve and the "GPU preview" label
- Windows and Linux functional checks of the fallback and of correctness within the limits, not run
- A drag while queued reference exports hold the shared pool misses 16 ms p95 (18.7 ms at 60 MP) and 32 ms at 24 MP; the reduced-pool proposal in [instant previews](design/instant-preview.md#proposals-and-later-work) is unmeasured
- Behind Detail or an earlier Presence layer, Dehaze's light is computed with those layers left out, at rest, in motion and in export, the stand-in light the owner accepted on 2026-10-05 ([decisions](decisions.md#gpu-first-rendering)); five sharpen-stress cells under Dehaze −100 miss the limits in motion so ([performance](specs/performance.md#the-per-frame-lights-reduction-factor)). It is revisited if the final measurement or real photographs show a visible difference
- On a cold Metal shader cache, as after an update, a Presence drag's sequences can take several seconds to compile after a Presence commit ([performance](specs/performance.md#presence-and-detail-sequences-in-the-editor)). Warm-up compiles the open stack's drags after the picture on screen's programs ([design](design/gpu-preview.md#warming-at-launch-and-open)); a drag before they are ready holds its frame for half a second, naming `compiling`, and then the reference renderer draws a whole frame per tick. Measured on a fresh bundle's empty Metal cache opening the Air 2S (2026-10-06): the first warm-up 2.1 to 2.2 s, the GPU's first frame 1.1 to 1.3 s after the reference frame, and a later Presence commit's warm-up 0.51 to 0.53 s ([performance](specs/performance.md#the-first-picture-on-a-cold-shader-cache)); a drag begun inside that window is not measured
- At 100%, Detail beside all three Presence fields charges up to 1.12 GB in the largest window the owner's display holds, each operation a link of the chain with planes of its own; a window of nearly twice its pixels, such as an external display's, would pass the 2 GiB budget, and its drag is drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging"), sharp at rest. The slot's shared scratch pool leaves this chain as it is: Detail's noise-reduction planes and Presence's are of other formats
- At 100% the window grows with every chained spatial layer, 207 px on every side for each masked Presence link of Texture and Clarity, and every link's intermediate and planes and the scratch pool take its size: the paint harness's layout fits 8 masks at 100% in its own view and 5 in the corpus's centred view, and past that a drag at 100% is drawn from its reduced stage scaled to the view, the status bar saying "Softer while dragging" ([performance](specs/performance.md#the-100-window-grows-with-the-chain)). **Proposals**, not built ([design](design/gpu-preview.md#later)): A, the GPU window planner grows a masked link's need only where its mask's bounds meet what its output must hold (about 1.44 GB at 10 masks in the harness's view, 2.38 GB at 16); B, each link's intermediate and kept planes sized to its own output's need (1.88 and 3.53 GB alone); both together 1.07 and 1.65 GB. A makes the boundary's key depend on the masks' bounds, so the drafted mask's link keeps its full halo, and needs a bit-exact test that coverage is zero outside a mask's bounds
- Painting over 10 to 16 masked Presence layers at Fit draws every tick on the GPU but misses the 16 ms p95 target (22.8 to 24.4 ms): a tick's GPU work is 8.8 ms over 10 masks and 14.5 ms over 16, past a 120 Hz frame ([performance](specs/performance.md#what-a-tick-costs-the-gpu))
- Proposal, not built: evaluate the ring of a 100% region that only Clarity's and Dehaze's 4× reductions read in strips, reducing as each strip is evaluated, so Detail's and Texture's full-resolution planes are held over the region alone, at the cost of a strip schedule and about 38% more Detail and Texture work in the ring ([design](design/gpu-preview.md#later))
- The first stroke after a zoom to 100% or a pan over a frame already in hand waits 61 to 84 ms for its region's boundary; a boundary rendered alone when such a view settles is proposed
- Later work in the [design](design/gpu-preview.md#later): a pan at 100% without a draft drawn on the GPU at once, region padding, Detail's sharpening change in one channel

**GPU-first rendering** ([design](design/gpu-first.md), [plan](../tasks/rendering/gpu-first.json)). Decided 2026-10-04 after a hover readout's point sample through Detail, Presence and masks froze the editor for about four minutes on a 16 MP RAW, and after an audit showed the remaining cost of an ordinary edit is the CPU renderer and the machinery around it. Done: the readout is removed, owner tasks run off the update loop (iced 0.14 had run them on it), every launch writes a bounded log, and the release gate, `cargo xtask gpu-qualification`, measures the corpus on the reference and on the GPU at Fit, 33%, 50% and 100%. Every zoom draws its drags on the GPU, and the GPU draws the picture at rest, every boundary derived from the photograph's source it holds: the gate passes by the recorded default, the picture at rest within its limits against the reference on every cell, while a drag's frame at Fit and below 100% is past them against the picture at rest and the reference alike on 462 of 783 cells, reported and not gated, the owner question it leaves open ([performance](specs/performance.md#gpu-qualification-against-the-reference)). Also done: the histogram and clipping counts on the GPU, Dehaze's light per frame on the GPU, samples and export through GPU tiles, shader warm-up, the drags the CPU proxy and region drew drawn on the GPU, the reference renderer as the fallback for a machine without a GPU (a session without one dragging on the CPU proxy), and the CPU production paths retired, the CPU renderer reduced to the whole-frame reference. Correctness is a declared tolerance against that reference. Measured on the M4 on a quiet host against the 2026-10-04 baseline (2026-10-06, [performance](specs/performance.md#gpu-first-against-the-2026-10-04-baseline)): every drag measured draws each tick on the GPU within a display frame at Fit, 33% and 100%, under Dehaze at 60 MP and on a RAW white balance; a commit, a history return and Compare's exit put the stack on screen at once; a sample through three spatial segments answers in 92 ms at p95. Outstanding:
- The picture at rest and its counts after a commit at 60 MP, 5.6 to 5.7 s against the reference renderer's 1.7 s, and over a masked Presence layer on the Air 2S, 15 to 17 s against 1.3 s: drawn one tile a frame in 240 tiles of 512 px and 330 of 256 px, each tile's window reaching a 300 to 720 px lead and its halos beyond what it draws
- A heavy GPU export, 10.2 to 10.5 s at 60 MP against the reference export's 2.1 to 2.2 s, in 240 tiles of 512 px, and 0.94 to 0.97 s at 24 MP against 0.76 to 0.81 s
- A stroke over three masked Presence layers at 100% on the Air 2S, beside Detail and a global Presence, misses 16 ms at p95 (23.7 ms)
- `editor-latency` refuses a run with a held tick (a drag that creates a masked layer under Presence, a gesture after a RAW white-balance release), so a masked slider drag is not measured by the tool
- The native Windows and Linux checks

**CPU and memory efficiency** ([design](design/efficiency.md), [plan](../tasks/rendering/efficiency.json)). Done and measured, except the `dist` build profile, which the owner deferred until the timing runs other plans have outstanding are recorded.

**GPU memory accounting** ([plan](../tasks/rendering/gpu-memory.json)). Measure and bound the GPU resources outside the provisional 1088 MiB photo-texture ceiling (crop textures, overlays and backend staging) before any total-memory guarantee.

## Output

**JPEG export follow-ups.** JPEG export is delivered ([design](design/export.md)).
- Presets, resizing, output sharpening and other formats, each only by its own decision

## Library

**Browse, pick, develop** ([design](design/catalog.md)). The Select workspace, events and moments, picks, developing picks into the catalog, catalog folders and collections, Locate and resolving missing originals, removal and batch preset and export are implemented on the design's recorded defaults ([feature status](features.md)). What remains ([outstanding](design/catalog.md#outstanding)):
- Owner decisions on the design's proposals, P13's choice of how to meet the photographs view's time among them
- Recording the `catalog-measure` figures, the first browse from a card reader included, and meeting the `browse.view` target over 100,000 photographs
- A labelled corpus of real trips, bursts and brackets from several makes, to check events and moments against
- Native Linux and Windows runs of the folder and volume watchers
- In the desktop: Add a folder…, the card-connected notice, Send back, dragging photographs onto a catalog folder, moving collections between groups, changing a smart collection's query, and Locate original… in export's refusal
- Background availability checks, and a browse filter for a missing value
- Later: merging brackets to HDR and stitching panoramas as a merge source kind (not selected)
- Catalog portability and backup, carrying each catalog's derived-artifact directory with it (decision pending)

## RAW

**RAW follow-ups** ([continuous RAW editing](design/initial-raw.md)). Continuous RAW editing is delivered; the owner closed its broader qualification plan on 2026-10-06 ([remaining boundaries](design/initial-raw.md#remaining-qualification-and-decisions)).
- Bayer highlight latitude: the owner decided to retain it, as X-Trans does, if the rendering change on the supplied Z6 and Air 2S files shows no new highlight artefacts; not built, so RCD still clips each gained Bayer site at sensor white
- Measure the cost and accuracy of a clip-aware white-balance draft on Bayer developments, whose drag frames on highlight-clipped scenes are reported and not gated ([decisions](decisions.md#gpu-first-rendering)). The candidate is in [instant previews](design/instant-preview.md#popular-cameras)

**RAW looks** ([design](design/raw-looks.md), [plan](../tasks/raw/raw-looks.json)). Decided 2026-10-05. New RAW photos start from a Luxforge look instead of the bare neutral development.
- Phase 1 (built; native evidence outstanding): the Standard look, chosen on the corpus and approved by the owner, in every new RAW photograph's Original; a Look section with Standard, Neutral and Amount; a Settings row for the starting look
- Phase 2: Match camera, a tone curve and chroma gain fitted per photo to its embedded camera preview off the owner, as a first-open entry or on request

**Camera coverage follow-ups** ([popular camera support](design/popular-camera-support.md)).
- Nikon High Efficiency NEF once upstream LibRaw decodes it; refused explicitly until then
- Sony A7 V compressed ARW once a pinned decoder reads it

## Editing tools

**Tone curve follow-ups** ([design](design/tone-curve.md)). The Tone curve is delivered ([feature status](features.md)).
- Owner review of the recorded defaults: what the composite acts on, channels, order, endpoints, the point limit, the Lightroom transfer and the editor gestures; below black the curve uses a floor-subtracted ratio (decided 2026-09-30)
- Follow-up: whether Basic's Blacks adopts the same floor-subtracted ratio, since lifting Blacks turns near-black noise into coloured speckle

**Corrections** (proposal, [design](design/corrections.md), [plan](../tasks/editing/corrections.json)). Remove blemishes by hand; offline Clone/Heal has its own delivery and qualification.
- Decided: repair after source development and before colour, deletion of `PointReplace`, stale patches kept rendering with export acknowledgement. Open: repair versus Detail restoration placement, correction preset eligibility, Heal numerics, brush/source-edge behaviour and bounded candidate/export shapes
- Reuse the delivered typed stroke store and drafts, module/capability contracts, Develop/prepare/adopt, Locate/library cleanup and nonlinear geometry; add the production brush interaction and ordered repair module
- Integrate rendering against the merged GPU-first interfaces and comparison harness; qualify offline UI/API, catalog previews, single/batch export and native performance after feature work
- Separately extend the same repair layer with frozen patches, bounded candidate jobs and source plus intra-layer prefix invalidation for AI editing; no model/provider prerequisite for the offline milestone

**AI editing** (decided 2026-10-05, [design](design/ai-editing.md), [plan](../tasks/editing/ai-editing.json)). Local, user-downloaded models behind one inference port and one analysis cache; nothing bundled, nothing sent without consent. The plan runs once the GPU-first integration branch has merged, with the Corrections foundation beside its first stage.
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
- RAW white balance import through a calibrated conversion from Lightroom's Kelvin and tint
- Copy and Paste Settings over the same composite action

**Lightroom import** (decided 2026-10-06, planned, not authorized; [design](design/lightroom-import.md), [plan](../tasks/lightroom/lightroom-import.json)). Bring a Lightroom Classic catalog or a folder of XMP sidecars across, read-only.
- Phase 1: the photographs worked on, catalog folders by event, collections, ratings and labels as collections, virtual copies and snapshots as versions, global settings, crop, orientation, lens and the look, the report and re-mapping, Lightroom's previews as first tiles
- Phase 2: masks and local adjustments; phase 3: spot removal, AI masks, Detail and perspective as their tools land

**Lightroom alignment** (decided 2026-10-06, planned, not authorized; [design](design/lightroom-alignment.md), [plan](../tasks/lightroom/lightroom-alignment.json)). Every supported slider follows Lightroom Classic's response, with targeted algorithm changes where a dimension still differs widely, in order of editing area: Basic, then Presence, the mixer and the vignette, then Detail, then masks and geometry, then RAW white balance.
- Open: authorization
- The rig (generated XMP, one Lightroom import and export per round by the owner), round 1 of single settings and round 2 of pairs, calibrated mapping rows as the importers' bridge, a response change per setting, targeted behaviour changes, validation on the owner's edited photographs

**Masks** ([design](design/masking.md)). Local adjustments. All four phases are delivered: the mask model and its persistence, the `mask.*` command family, the masked colour and spatial primitives, the Mask mode and panel, the coverage overlay — including for a mask that reads pixels, which [proposal P16](design/range-study.md#proposals) settled by reading the input of the mask's first bound layer once per display cell, measured and inside the preview budget, with the refusal kept where there is no operation to read or where reading one would cost a tile per cell — both gradients, brushes over the content-addressed stroke store, and the non-AI luminance and colour range selections with the colour-constrained brush.
- [Interaction repairs](design/masking-interactions.md): live candidate coverage, coherent selection and brush targets, unplaced/exclusive creation, `O` visibility and bounded live capture are implemented and verified. Native RAW liveness and scoped 24/60 MP hover/paint measurements pass; [performance](specs/performance.md#native-masking-interaction-qualification) records the memory and diagnostic limits
- [Mask performance](design/mask-performance.md) reduces overlay handoff and preview-worker cost within the existing buffer bounds. General photo/coverage tail latency remains open; the [matched native comparison](specs/performance.md#mask-feedback-and-the-coverage-handoff) keeps loaded runs and delayed positions visible
- A paint gesture's latency misses the provisional p95 bound on every recipe measured; the figures and their scope are in [performance](specs/performance.md#a-painted-strokes-own-latency)
- The range selections have no photographic corpus: every figure in the [range study](design/range-study.md) is over flat synthetic patches
- Density, edge-aware refinement, copying masks between photographs and mask presets are out of scope with their reasons recorded; model-based selections are the [AI editing](design/ai-editing.md) proposal's Select

**Tuning delivered tools.** Refine the recorded defaults of Presence, the colour mixer and the vignette (decision pending).

## Interface

**UI themes** ([design](design/ui-themes.md)) are implemented, with Omarchy import and six bundled Omarchy themes ([feature status](features.md)). Outstanding:
- Owner review of the recorded defaults: which themes are bundled, the contrast floors, the Omarchy forms read, the canvas background's Theme default and the Appearance tab
- Later, if the owner wants them: following Omarchy's current theme on Linux, following the system's light or dark appearance

## Programmability

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
- Measure optional-module activation cost
- Choose the first use case (decision pending)
- Loader proof with a real process or ABI trust boundary

## Inspection

**Performance panel follow-ups** ([design](design/performance-panel.md)). The Performance section, the activity board and the resource counters are delivered.
- GPU time and allocations on Linux (DRM `fdinfo`) and Windows (D3DKMT), and native checks of the CPU and memory counters there
- Attribute memory to the prepared source, the proxy and the GPU textures in `resources.read`
- Reduce the open section's whole-window redraw cost: idle model updates and unchanged GPU writes are cheaper, but an aggregate idle CPU improvement is not established
- A rendered frame of a RAW development while it runs

## Platform and release

**Full-editor verification.** Native M4 handoff of the complete editor, then Windows and Linux.

**Cross-platform builds.**
- macOS and Linux CI with GUI smoke results and artifact retention; Windows CI is disabled and Windows support will come later
- Windows and Linux packaging, checked in real desktop sessions
- Reproducible Linux VM route
- Developer guide checked on Windows and Linux

**Dependencies.**
- Remove or re-review the ttf-parser (by 2026-10-29) and paste (by 2026-12-18) advisory exceptions ([plan](../tasks/project/dependency-advisories.json))
- Automated license, asset and advisory checks; the manual review stays deferred

## Not in scope

Map, Book, Slideshow, Print, Web and Publish Services. Accounts, cloud sync, built-in AI chat, a plugin marketplace, a public compatibility framework and a generalized processing graph. General bitmap layers with blend modes. AI denoise, upscaling, generative expand, text-to-image and face retouching are not proposed ([AI editing](design/ai-editing.md) covers removal, selection, generative fill and sky replacement).
