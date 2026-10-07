# Code structure consolidation

Status: implementation authorized by the owner on 2026-10-07, in progress. The [task plan](../../tasks/project/code-structure.json) tracks acceptance. The owner approved the static `luxforge-gpu-types` plus `luxforge-gpu` split on 2026-10-07; TASK-003 is accepted; the checked boundary governs extraction.

## Objective

Remove repeated contracts and redundant preparation, make current Rust interfaces coherent, and separate reusable GPU execution from presentation. Preserve the current image, command, catalog, failure and scheduling behavior. Success is fewer independent definitions and fewer places rebuilding invariant-sensitive rules, rather than a lower file or line count.

The core continues to own transactions, history, validation and bounded services. Modules continue to declare semantic operations. GPU correctness remains the declared tolerance against the whole-frame CPU reference, with the independent numerical reference isolated from production code. Current formats only; no compatibility adapters, migrations, plugin ABI or generalized render graph.

## Current structure and findings

The dependency graph already has valuable boundaries: the core has no production GPU or GUI dependency, widgets do not import core, the JSON CLI builds without the GUI/GPU stack, and the numerical reference cannot import a workspace crate. Host parameter declarations generate parsing and schemas together; the method table owns dispatch; field-patch modules share validation and planning; the rendering pipeline shares control flow through its pixel domain; and `Latest` provides a shared bounded worker primitive. Preserve these boundaries and mechanisms.

The current GPU path is core compilation/planning, desktop lowering, and execution under the UI's photo surface. The window-free `TileRunner` shares the production pipeline with display, while the desktop's tile worker adapts it to the core's `TileService`. Export, samples and analysis now use this execution as well as display.

| Finding | Current issue | Outcome | Tasks |
| --- | --- | --- | --- |
| R1 | Delivered: public `Provider::descriptor` reads registry availability; explicitly dereferenced module hooks retain raw metadata | One registry-aware availability answer through the public lookup surface | TASK-001 |
| R2 | Delivered: core and UI consume common formats, extents, pass shapes and shader constants from `luxforge-gpu-types`; executable light references remain in the backend | One shared primitive contract; semantic planning and device lowering keep their distinct responsibilities | TASK-003, 004 |
| R3 | Delivered: physical plane placement, scratch-pool maxima, extents, tail precision, parameter sizing/alignment and output/buffer buckets share pure rules | Prediction and allocation use shared rules with explicit device inputs and actual-allocation checks | TASK-005 |
| R4 | Window-free GPU execution is reusable but lives in the widget subsystem | One core-free, Iced-free executor with a thin presentation adapter | TASK-006, 007 |
| R5 | Delivered: supported `ToolModule` authors have context helpers and plan builders; field-patch authoring stays internal and the developer proof has an opaque factory | A coherent current cross-crate surface, with internal authoring machinery clearly internal | TASK-009 |
| R6 | Stream strategy and tile-size selection repeatedly prepare the same GPU stack, and staged selection discards a newly generated tile list | One preparation per equivalent worker-scoped request; derive alternatives and reuse sweep tiles | TASK-008 |
| R7 | Delivered: colour and spatial operation equality uses exact unit kind, coefficient and stage bits; diagnostics are independent | Explicit exact semantic identity independent of diagnostic wording | TASK-010 |
| R8 | Preview selection and reference/Fit/region intent are explicit; resource phases still use loose flags/options and repeated eligibility checks | Small resource-phase types; one presentation/counts eligibility decision | TASK-011 delivered; TASK-012 remains |
| R9 | Delivered: one typed preview-read seam. Bounded decode/handle-cache mechanics still repeat | Shared mechanisms with separate grid/loupe policies and budgets | TASK-013, 014 |
| R10 | Delivered: renderer-only thresholds, frame bounds and tile rules live in core rendering | Renderer-only policy owned by core rendering; shared adapter admission stays with its actual owner | TASK-015 |
| R11 | Delivered: indexing and Locate share one hidden-file predicate and retain separate traversal policies | One hidden-status helper, separate traversal policies | TASK-016 |
| R12 | Delivered: comments describe current GPU-first paths and whole-boundary proof helpers are test-only | Current GPU-first contracts and accurate proof-only boundaries | TASK-002 |

Registry-aware provider access is verified from outside core, including disabled built-ins and all lookup paths. R6 is confirmed redundant construction, not a measured speedup. Preview requests/resource phases still need explicit intent; no delivered rendering failure was established. R3 is duplicated ownership, not evidence of a budget overrun. The relevant source entry points are linked in each task.

## GPU types and ownership

The owner approved two small static workspace boundaries:

- A pure internal GPU contract, `luxforge-gpu-types`, for common formats, plane-size/extent rules, pass shapes, shader conventions and resource-layout data/functions. It imports neither core, Iced nor wgpu. Core planning, the backend and desktop lowering can consume it.
- A wgpu backend, `luxforge-gpu`, for shader composition/compilation, execution, common allocation and retirement, and the window-free tile runner. It imports the contract and pinned GPU dependencies, never core or Iced.

The core retains semantic recipe compilation, stage/window planning and the `TileService` contract. The desktop retains host composition, core-to-device lowering, source/evaluation ownership, adapter policy and client/worker scheduling that depends on core requests. Lowering has one owner and is shared by display and tile work. The UI retains Iced device integration, presentation, frame pacing and display-specific scheduling. Share the device supplied by Iced rather than introducing a second device merely to separate crates.

Semantic core plans and executable device plans are not identical: lowering resolves light references, runtime words, bindings and geometry. Share genuinely common primitives instead of forcing both whole plan types into one representation. The existing `PlaneSize::Light` backend distinction remains explicit where required.

Each extraction moves implementation and updates its callers in the same task. Do not keep a parallel old executor or compatibility layer. A thin current presentation adapter is a supported boundary, not a historical interface. Repository source/dependency checks move with ownership and continue to refuse forbidden dependencies. Qualification must exercise the same production kernels after the move.

### Approved boundary (TASK-003)

Use `luxforge-gpu-types` and `luxforge-gpu` as static workspace crates. Neither exposes an external plugin ABI. This boundary is based on current `main` (`3ac13023`); there is one existing execution family, not an executor to replace.

| Owner | Current implementation and resulting responsibility |
| --- | --- |
| `luxforge-gpu-types` | Common `BoundaryFormat`, `PlaneFormat`, reduced/fixed plane extents, precision/channel compatibility, `PassShape`, words/block headers, pass parameter indices/stride, binding indices, workgroup dimensions and shared WGSL prelude/stubs. Pure resource-layout functions describe scratch grouping, kept/intermediate/output extents, size buckets and parameter slices. No recipe, executable whole plan, device, shader compiler or policy budgets. |
| `luxforge-core` | Module-owned WGSL and semantic descriptions (`render/gpu/program.rs`, `spatial.rs`), recipe compilation, mask sampling, stage/window walk, chained/staged/light-sweep planning, conservative admission and `tiles::TileService`. Semantic planes retain their scratch declarations; a semantic fixed one-texel light is resolved by lowering rather than conflated with a runtime resource reference. |
| `luxforge-gpu` | Existing `gpu_preview` shader assembly, `compile`, `chain`, `spatial`, `source`, `light`, `staged`, `rest` and `histogram` execution/allocation; executable plans, sources, buffers and resource slots; bounded compile/pass caches; GPU availability/loss and resource charge/retirement mechanisms; existing `tiles::TileRunner` and worker readback. Backend plane sizing uses the common reduced/fixed extent plus an explicit resolved `Light(index)` reference. |
| `luxforge-app` | The sole semantic-to-executable lowering in `app/gpu_plan.rs`, shared by display, tile reads, export and catalog tiers; core-aware evaluation/source lifetimes, stream strategy selection, client cancellation, worker/band scheduling, output encoding installation, adapter selection/refusal policy and complete reference restart. |
| `luxforge-ui` | `PhotoSurface` and Iced `shader::Pipeline` implementation, texture placement/bindings for presentation, crop/overlay/photo presentation and its separate photo-texture charges, redraw/dissolve pacing and UI diagnostics projection. It hands the backend the device/queue supplied by Iced and borrows the backend output view; it does no recipe lowering. |
| Qualification | Backend headless execution/readback helpers and kernel tests move with the executor, behind test-only `qualification`. Core WGSL tests use the common convention. App tests/corpus harness compare the production backend against core/reference and exercise host cancellation/fallback. The independent numerical oracle remains workspace-independent. |

Production dependencies are exactly these workspace edges: `core -> gpu-types`; `gpu -> gpu-types`; `ui -> gpu`; `app -> core + ui + gpu`. The UI may import the pure contract directly only where its presentation needs a common format. The contract uses `std` only. The backend uses the already pinned `wgpu = 27.0.1` (`wgsl`, `naga-ir`), `half = 2.7.1` (`std`) and `bytemuck = 1.25.2`, moving the existing pins rather than adding another stack. `iced` and `iced_wgpu` stay in UI/app. Test-only dependencies (`luxforge-testbase`, `luxforge-reference`, `luxforge-process`) remain test-only; backend `qualification` enables only its existing bounded headless wait helper. The CLI still reaches core and the pure contract, with no GPU/GUI dependency. Repository rules enforce these transitive boundaries and move qualification/source ownership checks with the implementation.

The concrete presentation seam is an executor constructed from borrowed/cloned `wgpu::Device`/`Queue`, a neutral wake callback, and explicit device limits/target format. Its per-surface execution state groups the existing slot, lights, rest accumulator and histogram resources; preparation returns output view/size, readiness/failure and figures. Iced's adapter keeps surface IDs, presentation uniforms, dissolve and redraw decisions. The current coupling to `PhotoPipeline`, `Picture`, `SurfaceSlots`, `SurfaceFigures` and `wake_surface` is replaced at this seam, not carried into the backend. Neutral wake callbacks replace imports from `photo_surface` in `compile`.

Retirement uses one device-scoped blocking-idle worker, preserving its existing 2 ms nonblocking poll only while work is outstanding. Its wgpu completion/retention mechanism lives in the backend; neutral retained resources and completion callbacks allow the UI's photo budget to discharge independently from the preview budget. There is no extra polling worker or early discharge. Presentation keeps its distinct current/retiring-photo admission policy.

Device sharing means display uses Iced's existing device. The **existing** tile worker continues to open its separate worker device on the window's matching backend/name, with the same requested limits and adapter-mismatch refusal; extraction introduces no additional device. The wgpu enumeration/open mechanism can move core-free to the backend, but software-adapter adoption and launch policy remain host choices. Worker waiting and display's nonblocking submission remain separate.

Layout takes boundary origin/size, format, plane roles, link/tail/output requirements and buffer lengths; actual storage offset alignment and texture/binding limits are explicit device inputs. The current 256-byte pass stride is retained and checked against supported device alignment. Logical core estimates, actual resident allocation and retiring allocation remain separate. Keep the existing conservative light allowance explicit; the desktop's unconverted estimate is not an admission upper bound. Preserve today's 2 GiB preview budget, 2 GiB tile-worker budget, 256 MiB boundary cap, 256 MiB read reserve, two tiles/two export bands in flight and all cache caps. Crop/overlay/backend staging remain outside the existing accounting scope; this proposal claims no total-memory guarantee.

The owner approved this boundary and the `-types` name on 2026-10-07. TASK-004 through TASK-007 and TASK-008 start according to their existing dependency graph after TASK-003 checks pass. A consequential boundary change still requires owner review and a validated design/DAG.

### Resource layout

`luxforge-gpu-types::layout` owns physical plane classes, kept/scratch/light placement, per-link scratch counts and pool maxima, texture/parameter bytes, tail precision and output/buffer buckets. Core prediction and backend allocation use those rules. Device side/binding limits, fixed-stride alignment and policy ceilings are explicit inputs; whole photo presentation retains its separate tiled filtering aprons. Actual backend admission still guards resource creation.

Keep logical estimates, resident allocations, retiring resources, upload/readback staging and process RSS distinct. Existing light-buffer allowances must be explicit; do not turn a texture-only estimate into a total-memory guarantee. The desktop's unconverted fallback estimate omits parameter slices and is not an upper bound: it must not replace conservative admission. Existing total-memory work remains separate, and no accounting scope or budget value changes without its own decision and evidence.

### Worker-scoped preparation

Prepare the compilation and source-derived GPU plan once for an equivalent stream request, then derive chained tile grids, staged sweeps and light sweeps from that value. Reuse the chosen sweep's tiles directly. A reduced stage, different mask sampling or different draft semantics can legitimately require a different compilation; reuse only when those semantics match.

Preparation lives only as long as the current read/stream. An evaluation retains its source's memory gate, so it must not become desktop state or an unbounded global cache. Preserve cancellation, bounded in-flight bands, device-loss behavior, output order and complete reference restart for an export that fails on the GPU.

## Current module and operation interfaces

Make the registry-aware provider descriptor public and test it from outside core. Assess `Deref` deliberately: explicit access to raw hooks must not obscure which descriptor carries registry availability. Every public lookup should clearly state whether it returns a registered provider or raw module implementation.

Audit actual cross-crate producers and consumers before changing visibility. Field-patch built-in machinery that has no current external author belongs behind `pub(crate)`; public inputs/outputs used by consumers remain public. Where an existing supported authoring path needs builders, value readers or context invariants, provide a complete small interface and prove it through an external compile/test. External loading and a general plugin authoring framework remain later work; discovering a new authoring requirement does not authorize building it.

Give colour/spatial units an exact semantic identity separate from `describe`. Identity includes the unit kind and every coefficient or stage-dependent value that affects processing. Equality must not depend on a lossy hash. Preserve current conservative mask allocation identity unless separately justified. Diagnostic wording and formatting must be free to change without changing operation equality.

## Preview and cache interfaces

`PreviewRequest` carries `PreviewSelection` (whole or prefix of a committed stack or client draft) and `PreviewRenderIntent` (reference, GPU Fit or GPU region). Fit carries required bounds; a region carries its magnification, reduction threshold and optional reference proxy. Prefix analysis retains its validation refusal; GPU draft prefixes refuse explicitly rather than dropping the GPU request. Session ownership is checked first. Valid requests, source preparation, quality and fallback policy are unchanged.

Group resource identity with readiness and phase, and share the decision for presenting settled tiles versus obtaining counts. Use small related structs/enums rather than one exclusive global GPU state: Compare, retained content, warm work and counts can overlap. Keep generation rejection, crop behavior, dissolve, compiling timeout and off-owner work unchanged.

Grid and loupe first share a typed `preview.read`/error-decoding helper, then a small decoded-handle/cache mechanism for source key and side matching, byte/count accounting, stale result acceptance, protected-entry eviction and one-time handles. Keep grid revision handling and cell planning separate from loupe tier/role selection, cancellation and lookahead. Retain independent budgets, worker limits and priority policy; do not combine controllers or add a general cache framework.

## Small ownership and documentation fixes

Move renderer-only `RenderPass` thresholds, RGBA frame limits and spatial tile/halo rules from RAW into core rendering without changing their values. Leave RAW source/development admission and genuinely shared adapter bounds with their actual owners; update repository rules and imports together. This move needs no new crate.

Share dot-file/macOS `UF_HIDDEN`/Windows hidden-attribute detection within core. Index and Locate traversal exclusions remain independent.

Correct gesture-only GPU comments and obsolete dead-code expectations. Trace any supposedly unused helper before deletion; preserve or relocate proof/qualification helpers behind their real boundary. Comments must not turn a still-supported no-GPU fallback into an abandoned path.

## Scope limits and invariants

Preserve the distinct production GPU, whole-frame CPU fallback/reference and independent numerical oracle. Preserve byte/JPEG and linear/RAW quantization and source semantics; reduced-source motion versus process-first settled tiles; chained/staged/light-sweep memory strategies; and nonblocking display versus worker readback pacing. Numerical Rust and WGSL implementations remain separately testable.

Source files, recipes, immutable history, command schemas, declared output tolerances, slider responses, GPU budgets, private camera inventory and existing platform support do not change. No UI controls, new JSON/MCP operations, dynamic module loading, CLI GPU enablement, generalized node graph, release framework or public compatibility layer is added. A changed format rejects unsupported data explicitly rather than rewriting it; no format bump should be needed for this structural work.

## Open decisions

1. **GPU crate boundary:** the owner approved the pure `luxforge-gpu-types` plus Iced-free `luxforge-gpu` split on 2026-10-07, with the device-sharing seam and ownership above. Implementation must stay within that boundary.
2. **Current Rust authoring surface:** default to supported current consumers and keep built-in-only machinery internal. TASK-009 inventories usage. If this exposes an actual requirement for new external authors, take that scope change to the owner rather than silently turning a visibility cleanup into a plugin framework.
3. **Native qualification prerequisites:** the authentic-source manifest and native hosts must be available for the corresponding TASK-017 checks. Missing scope stays open; timing starts only after implementation is integrated.

## Performance review of the contained cleanups

Provider lookup, current-contract comments, module surface visibility, typed preview reads, renderer policy ownership and hidden metadata sharing add no original read/hash/decode path, frame allocation, render in a query/validation/no-op check, owner-thread frame work, desktop refresh/upload, timer, poll, subscription or cache. Existing source verification, worker limits, JSON request data and traversal policies are retained. The moved policy values are unchanged. Exact rendering/admission regressions and protocol/traversal tests cover these changes; no image algorithm changes. Photo-sized timing and allocation comparisons remain TASK-017 work on the integrated branch, so these cleanups make no speed or memory claim.

## Acceptance and verification

Every task has observable acceptance and scoped tests in the JSON plan. The primary outcomes are: one registry availability answer; one common GPU format/extent/convention definition; common layout rules checked against actual allocations; one production executor; one equivalent stream preparation; clear supported module construction/read interfaces; explicit preview intent; and shared cache mechanics with preserved policy.

While implementing, run only the affected scoped tests. Run `quick` once at each task handoff and rendered checks at GPU/UI integration points, using the compiler-cache wrapper on macOS/Linux and background editor launches. Update task check commands when tests move to an actually created package; proposed package names are not commands that exist today. Every output directory is fresh.

The integrated qualification task runs after feature work: native M4 comparison within the existing tolerances for display, histogram, samples, catalog tiers and export; exact CPU/reference regression checks; source preservation and unavailable-provider recovery; correlated state/logs/captures for preview, Compare/crop, grid and loupe; cancellation, budget refusal, retirement and device-loss checks; and existing portability/no-GPU checks. An incomplete or software-adapter run is recorded with its scope, never treated as native evidence.

Measure once on the finished branch against a baseline built separately from the pre-refactor revision: stream setup, representative 24/60 MP commit/edit/export behavior and relevant peak allocations. Use existing harnesses where possible, the specified sample counts, a quiet host and the performance rules. Compile/plan counters demonstrate eliminated preparation; no numerical speed or total-memory claim is made without measurement. Full verification with the authentic-source manifest is required before programme completion; unavailable private fixtures or native hosts leave the corresponding acceptance open.

Finally update architecture/module/rendering documents and feature status to the delivered shape. Update the user guide only if documented behavior or an interface explanation actually changes. Delete the completed task plan and its roadmap/index entries under the repository conventions; this design retains only the current outcome and outstanding decisions.
