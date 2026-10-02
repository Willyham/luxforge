# Roadmap

Outstanding work by area. What is delivered is in [feature status](features.md); pillars in [AGENTS.md](../AGENTS.md); accepted decisions in [decisions](decisions.md). Relative priority needs owner input ([open questions](decisions.md#open-product-questions)).

## Engineering

**GPU previews** ([design](design/gpu-preview.md), [qualification](specs/performance.md#gpu-previews-qualified-on-the-m4)). Implemented and qualified on the M4; outstanding:
- Owner review of the proposed error limits, the 150 ms dissolve, the "GPU preview" label and the `gpu_preview` preference
- Windows and Linux functional checks of the fallback and of correctness within the limits, not run
- A drag while queued exports hold the shared pool misses 16 ms p95 (18.7 ms at 60 MP) and 32 ms at 24 MP; the reduced-pool proposal in [instant previews](design/instant-preview.md#proposals-and-later-work) is unmeasured
- At 100%, Detail beside Presence (Dehaze's estimate behind Detail) and the stacks past the 640 MiB budget keep the CPU path
- Later work in the [design](design/gpu-preview.md#later): view changes without a draft, the RAW white-balance draft on the GPU, region padding

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
- Measure the cost and accuracy of a clip-aware white-balance draft on Bayer developments, then reconsider it against the highlight-clipped exception to the `raw-panel` gates. The candidate is in [instant previews](design/instant-preview.md#popular-cameras) and the exception in [decisions](decisions.md#raw-white-balance-drafts)

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
- Off-owner pixel queries and mutations behind a spatial prefix, the 16-bit JPEG hand-off (which also changes Presence), a restoration-prefix proxy cache and an input-grid overlay cache
- Generated controls/API, masks, native presets and history
- Approximate motion, exact-derived settled Fit and 100% inspection
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
