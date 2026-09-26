# Roadmap

Outstanding work by area. What is delivered is in [feature status](features.md); pillars in [AGENTS.md](../AGENTS.md); accepted decisions in [decisions](decisions.md). Relative priority needs owner input ([open questions](decisions.md#open-product-questions)).

## Engineering

**After the consolidation** (authorized 2026-09-26; [design](design/post-consolidation.md)). Finish one path per concept, fix the defects the post-consolidation review found, take the byte-identical speedups and trim the tests. One plan per group: [core service](../tasks/core-service.json), [module contract](../tasks/module-contract.json), [rendering](../tasks/rendering.json), [RAW](../tasks/raw.json), [masking](../tasks/masking.json), [capabilities](../tasks/capabilities.json), [desktop](../tasks/desktop.json) and [harness](../tasks/harness.json). The waves run in order, overlapping where they touch different files:
1. Fix: the verified defects, each with a regression test
2. Finish the consolidation: one path for every concept that still has two, each kept single by a repository check
3. Speed: the byte-identical speedups on the preview, RAW-development and gesture paths, each measured before and after
4. Structure: the simplifications that follow once each path is single
5. Tests: each property proven once per layer, and fewer test binaries
6. Roadmap groundwork, done first when its milestone below starts: MCP (`events.wait`, typed host parameters, the headless binary's crate), the library and Locate (per-photo selection, a paged catalog, relocation, surface ids), JPEG export (an export job lane on the bound evaluation), Corrections (stage-boundary methods, per-tile input regions, generic path primitives), and a new geometry module (a carry hook)

## Output

**JPEG export.** Write a recipe's render to a new file without touching the original.
- Colour and metadata contracts, proven on fixtures
- Export from a snapshot: quality 90, no overwrites, metadata stripped by default with a Keep metadata option
- Desktop and API controls
- RAW recipes through the same exporter

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

## Editing tools

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
- A paint gesture's latency misses the provisional p95 bound on every recipe measured; the figures and their scope are in [performance](specs/performance.md#a-painted-strokes-own-latency)
- The range selections have no photographic corpus: every figure in the [range study](design/range-study.md) is over flat synthetic patches
- Density, edge-aware refinement, model-based selections, copying masks between photographs and mask presets are out of scope with their reasons recorded

**Tuning delivered tools.** Refine the recorded defaults of Presence, the colour mixer and the vignette (decision pending).

## Programmability

**MCP adapter.** Expose the whole operation registry to agents through a standards-compliant MCP server over the existing command service.

**Shared editing** (future, [design](design/shared-editing.md)). One host, one invited collaborator or agent, one photograph. No milestone.

## Extensibility

**Shared module capabilities follow-ups** ([design](design/module-capabilities.md)). Settings and secrets, consent, the transport, activation and resources, tasks and derived artifacts are delivered on macOS.
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
- Publish export jobs to the activity board when export lands, as capability jobs already publish with their progress
- Cancel listed work from the section, through the cancel each job already has
- GPU time and allocations on Linux (DRM `fdinfo`) and Windows (D3DKMT), and native checks of the CPU and memory counters there
- Attribute memory to the prepared source, the proxy and the GPU textures in `resources.read`
- Remember whether the section is collapsed, in the host's user-level settings
- Lower the cost of the open section's one-second redraw, which now counts in the idle figure
- A rendered frame of a RAW development while it runs

## Platform and release

**Full-editor verification.** Native M4 handoff of the complete editor, then Windows and Linux.

**Cross-platform builds.**
- Three-platform CI with GUI smoke results and artifact retention
- Windows and Linux packaging, checked in real desktop sessions
- Reproducible Linux VM route
- Developer guide checked on Windows and Linux

**Dependencies.**
- Remove or re-review the ttf-parser (by 2026-10-19) and paste (by 2026-12-18) advisory exceptions ([plan](../tasks/dependency-advisories.json))
- Automated license, asset and advisory checks; the manual review stays deferred

## Not in scope

Map, Book, Slideshow, Print, Web and Publish Services. Accounts, cloud sync, built-in AI chat, a plugin marketplace, a public compatibility framework and a generalized processing graph. General bitmap layers with blend modes. Generative editing, pending the owner's review of the Corrections proposal.
