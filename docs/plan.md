# Roadmap

Outstanding work by area. What is delivered is in [feature status](features.md); pillars in [AGENTS.md](../AGENTS.md); accepted decisions in [decisions](decisions.md). Relative priority needs owner input ([open questions](decisions.md#open-product-questions)).

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
- Catalog portability and backup (decision pending)

## RAW

**RAW qualification.** Make the [continuous RAW editing](design/initial-raw.md) that exists trustworthy ([plan](../tasks/implementation-initial-raw.json)).
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

**Masks** ([design](design/masking.md), [plan](../tasks/implementation-masking.json)). Local adjustments. The linear gradient, the mask model and its persistence, the `mask.*` command family, the masked colour and spatial primitives, the Mask mode and panel, and the coverage overlay are delivered.
- The radial gradient's handle editor, subtract and intersect, per-component and per-mask inversion, amount, reorder and duplicate
- Brushes over the delivered path primitives and the content-addressed stroke store
- Non-AI luminance and colour range selections
- Full-editor acceptance with masks in the journey

**Tuning delivered tools.** Refine the recorded defaults of Presence, the colour mixer and the vignette (decision pending).

## Programmability

**MCP adapter.** Expose the whole operation registry to agents through a standards-compliant MCP server over the existing command service.

**Shared editing** (future, [design](design/shared-editing.md)). One host, one invited collaborator or agent, one photograph. No milestone.

## Extensibility

**Shared module capabilities** (proposal, [design](design/module-capabilities.md), [plan](../tasks/module-capabilities.json)). Host services modules can rely on.
- Capability and consent contract (owner decision)
- Typed settings and secret storage
- Scoped permissions and protected transports
- Activation, resource jobs and verified model downloads
- Immutable derived-artifact storage
- Proof module and M4 measurements

**External modules.** Load separately authored modules.
- Measure optional-module activation cost
- Choose the first use case (decision pending)
- Loader proof with a real process or ABI trust boundary

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
