# Roadmap

Outstanding work by area. What is delivered is in [feature status](features.md); pillars in [AGENTS.md](../AGENTS.md); accepted decisions in [decisions](decisions.md). Relative priority needs owner input ([open questions](decisions.md#open-product-questions)).

## Engineering

**GPU previews** ([design](design/gpu-preview.md), [qualification](specs/performance.md#gpu-previews-qualified-on-the-m4)). Implemented and qualified on the M4; outstanding:
- Owner review of the proposed error limits, the 150 ms dissolve and the "GPU preview" label
- Windows and Linux functional checks of the fallback and of correctness within the limits, not run
- A drag while queued reference exports hold the shared pool misses 16 ms p95 (18.7 ms at 60 MP) and 32 ms at 24 MP; the reduced-pool proposal in [instant previews](design/instant-preview.md#proposals-and-later-work) is unmeasured
- Behind Detail or an earlier Presence layer, Dehaze's light is computed with those layers left out, at rest, in motion and in export, the stand-in light the owner accepted on 2026-10-05 ([decisions](decisions.md#gpu-first-rendering)); five sharpen-stress cells under Dehaze −100 miss the limits in motion so ([performance](specs/performance.md#the-per-frame-lights-reduction-factor)). It is revisited if the final measurement or real photographs show a visible difference
- On a cold Metal shader cache, as after an update, a Presence drag's sequences can take several seconds to compile after a Presence commit ([performance](specs/performance.md#presence-and-detail-sequences-in-the-editor)). Warm-up compiles the open stack's drags after the picture on screen's programs ([design](design/gpu-preview.md#warming-at-launch-and-open)); a drag before they are ready holds its frame for half a second, naming `compiling`, and then the reference renderer draws a whole frame per tick. Its cold-cache cost with warm-up is not measured
- At 100%, Detail beside all three Presence fields charges up to 1.12 GB in the largest window the owner's display holds, each operation a link of the chain with planes of its own; a window of nearly twice its pixels, such as an external display's, would pass the 2 GiB budget, and its drag is drawn from its reduced stage scaled to the view, the softer frame ("Softer while dragging"), sharp at rest. The slot's shared scratch pool leaves this chain as it is: Detail's noise-reduction planes and Presence's are of other formats
- At 100% the window grows with every chained spatial layer, 207 px on every side for each masked Presence link of Texture and Clarity, and every link's intermediate and planes and the scratch pool take its size: the paint harness's layout fits 8 masks at 100% in its own view and 5 in the corpus's centred view, and past that a drag at 100% is drawn from its reduced stage scaled to the view, the status bar saying "Softer while dragging" ([performance](specs/performance.md#the-100-window-grows-with-the-chain)). **Proposals**, not built ([design](design/gpu-preview.md#later)): A, the GPU window planner grows a masked link's need only where its mask's bounds meet what its output must hold (about 1.44 GB at 10 masks in the harness's view, 2.38 GB at 16); B, each link's intermediate and kept planes sized to its own output's need (1.88 and 3.53 GB alone); both together 1.07 and 1.65 GB. A makes the boundary's key depend on the masks' bounds, so the drafted mask's link keeps its full halo, and needs a bit-exact test that coverage is zero outside a mask's bounds
- Painting over 10 to 16 masked Presence layers at Fit draws every tick on the GPU but misses the 16 ms p95 target (22.8 to 24.4 ms): a tick's GPU work is 8.8 ms over 10 masks and 14.5 ms over 16, past a 120 Hz frame ([performance](specs/performance.md#what-a-tick-costs-the-gpu))
- Proposal, not built: evaluate the ring of a 100% region that only Clarity's and Dehaze's 4× reductions read in strips, reducing as each strip is evaluated, so Detail's and Texture's full-resolution planes are held over the region alone, at the cost of a strip schedule and about 38% more Detail and Texture work in the ring ([design](design/gpu-preview.md#later))
- The first stroke after a zoom to 100% or a pan over a frame already in hand waits 61 to 84 ms for its region's boundary; a boundary rendered alone when such a view settles is proposed
- Later work in the [design](design/gpu-preview.md#later): a pan at 100% without a draft drawn on the GPU at once, region padding, Detail's sharpening change in one channel

**GPU-first rendering** ([design](design/gpu-first.md), [plan](../tasks/gpu-first.json)). Decided 2026-10-04 after a hover readout's point sample through Detail, Presence and masks froze the editor for about four minutes on a 16 MP RAW, and after an audit showed the remaining cost of an ordinary edit is the CPU renderer and the machinery around it. Done: the readout is removed, owner tasks run off the update loop (iced 0.14 had run them on it), every launch writes a bounded log, and the release gate, `cargo xtask gpu-qualification`, measures the corpus on the reference and on the GPU at Fit, 33%, 50% and 100%. Every zoom draws its drags on the GPU, and the GPU draws the picture at rest, every boundary derived from the photograph's source it holds: the gate passes by the recorded default, the picture at rest within its limits against the reference on every cell, while a drag's frame at Fit and below 100% is past them against the picture at rest and the reference alike on 462 of 783 cells, reported and not gated, the owner question it leaves open ([performance](specs/performance.md#gpu-qualification-against-the-reference)). Also done: the histogram and clipping counts on the GPU, Dehaze's light per frame on the GPU, samples and export through GPU tiles, shader warm-up, the drags the CPU proxy and region drew drawn on the GPU, the reference renderer as the fallback for a machine without a GPU (a session without one dragging on the CPU proxy), and the CPU production paths retired, the CPU renderer reduced to the whole-frame reference. Correctness is a declared tolerance against that reference. Outstanding: the native Windows and Linux checks; the GPU sample's latency; one qualification and measurement on a quiet host at the end.

**CPU and memory efficiency** ([design](design/efficiency.md), [plan](../tasks/efficiency.json)). Done and measured, except the `dist` build profile, which the owner deferred until the timing runs other plans have outstanding are recorded.

**GPU memory accounting** ([plan](../tasks/rendering.json)). Measure and bound the GPU resources outside the provisional 1088 MiB photo-texture ceiling (crop textures, overlays and backend staging) before any total-memory guarantee.

## Output

**JPEG export follow-ups.** JPEG export is delivered ([design](design/export.md)).
- Presets, resizing, output sharpening and other formats, each only by its own decision

## Library

**Source recovery.** Keep edits reachable when originals move.
- Manual Locate through the UI and API, with verification

**Small library.** Work across many photos, not one ([decisions](decisions.md)).
- Multi-image import and virtualized browsing
- Filtering, tagging and collections
- Multi-selection and stacking
- Catalog portability and backup, carrying each catalog's derived-artifact directory with it (decision pending)

## RAW

**RAW qualification.** Make the [continuous RAW editing](design/initial-raw.md) that exists trustworthy ([plan](../tasks/raw.json)).
- High-precision development and neutral defaults on every qualified camera
- Foundation checkpoint across cameras, geometry and history
- Failure hardening: source, native worker, cache, recipe
- Packaged dependency delivery and portability
- M4 responsiveness, memory and JPEG regression measurements
- End-to-end RAW editing journey
- Measure the cost and accuracy of a clip-aware white-balance draft on Bayer developments, whose drag frames on highlight-clipped scenes are reported and not gated ([decisions](decisions.md#gpu-first-rendering)). The candidate is in [instant previews](design/instant-preview.md#popular-cameras)

**Camera coverage follow-ups** ([popular camera support](design/popular-camera-support.md)).
- Nikon High Efficiency NEF once upstream LibRaw decodes it; refused explicitly until then
- Sony A7 V compressed ARW once a pinned decoder reads it

## Editing tools

**Tone curve follow-ups** ([design](design/tone-curve.md), [plan](../tasks/tone-curve.json)). The Tone curve is delivered ([feature status](features.md)).
- Photo-sized measurement at 24 MP and 60 MP: the point drag to the presented frame, its settled histogram and the unit's frame cost
- Owner review of the recorded defaults: what the composite acts on, channels, order, endpoints, the point limit, the Lightroom transfer and the editor gestures; below black the curve uses a floor-subtracted ratio (decided 2026-09-30)
- Follow-up: whether Basic's Blacks adopts the same floor-subtracted ratio, since lifting Blacks turns near-black noise into coloured speckle

**Detail** (implemented; qualification in progress, [design](design/detail.md), [plan](../tasks/detail.json)). Manual noise reduction and capture sharpening before tone, on RAW and JPEG.
- Bounded numerical kernels and shared restoration/scale contracts
- Off-owner pixel queries and mutations behind a spatial prefix, the 16-bit JPEG hand-off (which also changes Presence) and an input-grid overlay cache
- Generated controls/API, masks, native presets and history
- Approximate motion, the picture at rest on the GPU and 100% inspection
- Photographic quality and native M4 cost qualification; output sharpening remains export follow-up scope

**Lens and perspective correction** (implemented; qualification in progress, [design](design/lens-and-perspective.md), [plan](../tasks/lens-and-perspective.json)). Offline Lensfun profile distortion and manual two-axis perspective, with a fixed covered canvas, shared nonlinear mapping for crop and masks, and explicit prevention of duplicate embedded DNG correction. Functional implementation is verified; performance and photographic qualification remain outstanding. Coverage and read bounds are closed forms; the pinned index ships as a separate resource; Perspective is not presettable and strong minification is refused. Qualification needs authentic photographs for the qualified camera, lens and focal combinations.

**Corrections** (proposal, [design](design/corrections.md), [plan](../tasks/corrections.json)). Remove blemishes and objects.
- Owner decisions: behaviour, repair-stage order, scope
- Offline Clone and Heal: numerical contract, repair stage, brush masks, desktop workflow
- AI Remove: provider qualification, local and remote adapters, candidate review and acceptance

**Presets follow-ups** ([design](design/presets.md#later)). The library, apply, create and Lightroom import are delivered.
- Owner review of the recorded defaults
- An Amount slider and a hover preview
- RAW white balance import through a calibrated conversion from Lightroom's Kelvin and tint
- Copy and Paste Settings over the same composite action

**Masks** ([design](design/masking.md)). Local adjustments. All four phases are delivered: the mask model and its persistence, the `mask.*` command family, the masked colour and spatial primitives, the Mask mode and panel, the coverage overlay — including for a mask that reads pixels, which [proposal P16](design/range-study.md#proposals) settled by reading the input of the mask's first bound layer once per display cell, measured and inside the preview budget, with the refusal kept where there is no operation to read or where reading one would cost a tile per cell — both gradients, brushes over the content-addressed stroke store, and the non-AI luminance and colour range selections with the colour-constrained brush.
- [Interaction repairs](design/masking-interactions.md): live candidate coverage, coherent selection and brush targets, unplaced/exclusive creation, `O` visibility and bounded live capture are implemented and verified. Native RAW liveness and scoped 24/60 MP hover/paint measurements pass; [performance](specs/performance.md#native-masking-interaction-qualification) records the memory and diagnostic limits
- [Mask performance](design/mask-performance.md) reduces overlay handoff and preview-worker cost within the existing buffer bounds. General photo/coverage tail latency remains open; the [matched native comparison](specs/performance.md#mask-feedback-and-the-coverage-handoff) keeps loaded runs and delayed positions visible
- A paint gesture's latency misses the provisional p95 bound on every recipe measured; the figures and their scope are in [performance](specs/performance.md#a-painted-strokes-own-latency)
- The range selections have no photographic corpus: every figure in the [range study](design/range-study.md) is over flat synthetic patches
- Density, edge-aware refinement, model-based selections, copying masks between photographs and mask presets are out of scope with their reasons recorded

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
- Owner review of the recorded defaults: per-asset photo consent, who may grant, loopback-only plain HTTP ([decisions](decisions.md#module-capabilities))
- Windows Credential Manager and Linux Secret Service for module secrets, verified natively; both refuse with `not-ready` today
- Native Windows and Linux checks of the transport's certificate verification and of resource removal, which on Windows must release a module's files before deleting them
- The first reviewed provider adapters and their crop and mask data classes, with Corrections
- Resumable, hash-checked downloads for large model files; an interrupted download restarts today
- Setting and clearing secrets off the catalog owner, so an OS keychain prompt never holds other clients
- `managed-storage` and `local-runtime` capabilities, when a module first needs them

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
- Remove or re-review the ttf-parser (by 2026-10-29) and paste (by 2026-12-18) advisory exceptions ([plan](../tasks/dependency-advisories.json))
- Automated license, asset and advisory checks; the manual review stays deferred

## Not in scope

Map, Book, Slideshow, Print, Web and Publish Services. Accounts, cloud sync, built-in AI chat, a plugin marketplace, a public compatibility framework and a generalized processing graph. General bitmap layers with blend modes. Generative editing, pending the owner's review of the Corrections proposal.
