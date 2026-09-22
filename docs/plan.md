# Roadmap

Lightwell is built history-first: prove non-destructive state, history and the programmable API, then add tools of increasing complexity. Pillars are in [AGENTS.md](../AGENTS.md), accepted decisions in [decisions](decisions.md) and per-capability status in [features](features.md).

## Milestones

| Milestone | Result | Status |
| --- | --- | --- |
| S0 — viewer | Cross-platform skeleton that opens one JPEG at Fit | Accepted |
| M1 — history foundation | Referenced JPEG in a SQLite catalog, ordered edit layers, a pixel-change proof, persistent history with undo/redo/restore/preview, catalog reopen, desktop UI and a live JSON/IPC API | Implemented, verified on M4 |
| M2 — transforms | Exact rotate left/right, mirror horizontal and flip vertical through the same layers, history, UI and API | Implemented, verified on M4 |
| M3 — tool modules | Declarative tool interface (actions, schemas, semantic controls, validation, processing); the pixel and transform tools use it through the shared command service | Implemented, verified on M4 |
| M4 — crop module | Lightroom-style crop and straighten as a module: free handles, ratios, straightening, Apply/Cancel, UI/API parity | Implemented, verified on M4 |
| Develop workspace | The single-image editing screen over the existing modules: widget crate, layered desktop, generated panels, canvas modes, notices, palette and per-client workspace state | Implemented, verified on M4 |

Each milestone starts from the previous working result and ends with a native M4 demonstration plus proportionate automated correctness and recovery checks. Contracts: [milestone design](design/history-first-roadmap.md), [history](specs/edit-history.md), [modules](design/modules-and-api.md), [crop](specs/single-image.md), [workspace](design/develop-workspace.md).

## Editor follow-ups

Implemented after M4: [content-space edits](design/content-space-edits.md) place pixel-stage edits before quarter-turns and the crop, with a content-coordinate pick and `render.locate`, and the [orientation layer](design/orientation-layer.md) folds repeated transforms into one layer.

After M4: JPEG export (quality 90, no overwrites, optional metadata stripped by default with a Keep metadata option, verified color and geometry), manual Locate for moved originals, a standards-compliant MCP adapter over the existing service, and complete native packaging and performance verification. Native Windows/Linux desktop checks and manual license reviews stay deferred until the owner asks.

## Later phases

Instant previews are implemented and verified on the M4 Mac: every preview job at Fit renders the recipe against a display-bounded proxy first and presents it through a surface that owns its texture, the exact render follows as a cancellable phase for the histogram, the overlays and the 100% view, and a gesture's round trip is synchronous, so a drained drag presents in 18 to 29 ms and a wild drag at 42 to 54 frames per second on 24 and 60 MP sources with every Basic unit active ([design](design/instant-preview.md), [measurements](specs/performance.md#instant-previews-proxy-phase-hop-rule-and-the-surface-primitive)). Basic adjustments (white balance with a neutral picker, exposure, tone, vibrance and saturation as one colour-stage layer) and the histogram inspector with clipping overlays are implemented and verified on the M4 Mac against the [design](design/basic-and-histogram.md) and its frozen [tone](design/basic-tone.md), [white balance](design/basic-white-balance.md) and [colour](design/basic-colour.md) studies. The recorded defaults the work ran on (provisional performance thresholds, the overlay rule, the picker patch, the float tolerances, one Basic layer) are the owner's to refine; measured misses are listed in [performance](specs/performance.md).

Relative priority still needs owner input; see the [open questions](decisions.md#open-product-questions).

Initial RAW implementation is requested, with a [design](design/initial-raw.md) and [task plan](../tasks/implementation-initial-raw.json) for the original Nikon Z6, Fujifilm X100VI and owner-supplied DJI Air 2S DNG. The owner requires continuous non-destructive RAW editing: retain high precision through the recipe, with display conversion at the end and JPEG only as explicit export. The implemented LibRaw/librtprocess pipeline retains sensor data and float development through source Exposure, White Balance, geometry and history. Owner NEF/RAF/DNG editing and reopen are verified through JSON and the background M4 renderer. The supplied FC3411 DNG uses the required gain-map and chromatic-warp corrections under the [Air 2S contract](design/air2s-dng.md). Broad controlled-scene, resource and portability qualification remain explicit.

| Phase | Scope |
| --- | --- |
| Useful small library | Multi-image import, virtualized browsing, filtering, tagging, collections, lazy shoot subsets, multi-selection and stacking, broader source recovery |
| Tonal editing and PNG | Exposure, white balance, contrast and related controls with explicit numerical and color contracts |
| Trustworthy RAW | [Continuous RAW editing](design/initial-raw.md) for Nikon Z6, Fujifilm X100VI and DJI Air 2S by actual mode; measured decoder/development selection, high-precision recipe evaluation and neutral exposure/WB |
| Richer tools | Texture, clarity, dehaze; separately scoped masks and clone/heal |
| External modules | Measured activation costs and a separately authored module loaded through documented host APIs |

## Future extension possibilities

[Shared editing](design/shared-editing.md) explores one host desktop, one invited collaborator or agent, and one photograph, using the shared command service and host-rendered previews. It is a future possibility with no scheduled milestone or implementation tasks. Collaborative undo, gesture sharing, permissions and conflict policy need explicit decisions; independent offline replicas and CRDTs are deferred until a workflow requires them. The proposal does not depend on cloud accounts or catalog sync.

## Not in scope

Map, Book, Slideshow, Print, Web and Publish Services. Accounts, cloud sync, built-in AI chat, generative editing, a plugin marketplace, a public compatibility framework and a generalized processing graph. General bitmap layers with blend modes and reordering: a "layer" here is an ordered recipe operation.
