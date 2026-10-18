# Code structure consolidation

Status: implemented and qualified on the native M4, including the static `luxforge-gpu-types` / `luxforge-gpu` boundary. [Qualification and measured scope](../specs/performance.md#code-structure-consolidation) records the complete available authentic corpus, current Linux functional checks and remaining native platform/memory gaps.

## Objective

The code keeps one definition of each repeated contract and one preparation of each value, coherent current Rust interfaces, and reusable GPU execution separate from presentation, without changing image, command, catalog, failure or scheduling behavior. The measure is fewer independent definitions and fewer places rebuilding invariant-sensitive rules, rather than a lower file or line count.

The core owns transactions, history, validation and bounded services. Modules declare semantic operations. GPU correctness is the declared tolerance against the whole-frame CPU reference, with the independent numerical reference isolated from production code. Current formats only; no compatibility adapters, migrations, plugin ABI or generalized render graph.

## Delivered structure

The dependency graph has these boundaries: the core has no production GPU or GUI dependency, widgets do not import core, the JSON CLI builds without the GUI/GPU stack, and the numerical reference cannot import a workspace crate. Host parameter declarations generate parsing and schemas together; the method table owns dispatch; field-patch modules share validation and planning; the rendering pipeline shares control flow through its pixel domain; and `Latest` provides a shared bounded worker primitive. Preserve these boundaries and mechanisms.

The current GPU path is core compilation/planning, desktop lowering, shared execution in `luxforge-gpu` and Iced presentation in the UI. The window-free `TileRunner` shares the production pipeline with display, while the desktop's tile worker adapts it to the core's `TileService`. Export, samples and analysis use this execution as well as display.

| Area | Delivered mechanism | Result |
| --- | --- | --- |
| R1 | Delivered: public `Provider::descriptor` reads registry availability; explicitly dereferenced module hooks retain raw metadata | One registry-aware availability answer through the public lookup surface |
| R2 | Delivered: core and UI consume common formats, extents, pass shapes and shader constants from `luxforge-gpu-types`; executable light references remain in the backend | One shared primitive contract; semantic planning and device lowering keep their distinct responsibilities |
| R3 | Delivered: physical plane placement, scratch-pool maxima, extents, tail precision, parameter sizing/alignment and output/buffer buckets share pure rules | Prediction and allocation use shared rules with explicit device inputs and actual-allocation checks |
| R4 | Delivered: execution and the neutral runner live in the backend; desktop lowering/worker/tests call it directly, and app owns renderer/software/refusal/limit policy | One core-free, Iced-free executor with a thin presentation adapter |
| R5 | Delivered: supported `ToolModule` authors have context helpers and plan builders; field-patch authoring stays internal and the developer proof has an opaque factory | A coherent current cross-crate surface, with internal authoring machinery clearly internal |
| R6 | Delivered: worker-scoped PreparedStream reuses the evaluation’s exact compilation and one source-derived GPU plan across strategy/tile-size choices; staged streams take the selected last sweep’s tile list | One preparation per equivalent worker-scoped request; derive alternatives and reuse sweep tiles |
| R7 | Delivered: colour and spatial operation equality uses exact unit kind, coefficient and stage bits; diagnostics are independent | Explicit exact semantic identity independent of diagnostic wording |
| R8 | Delivered: explicit selection/reference/Fit/region intent and per-resource pending/ready/refused conversion; one settled-picture/counts decision | Small resource-phase types; one presentation/counts eligibility decision |
| R9 | Delivered: typed preview reads and private shared decode/handle, key/side, accounting and protected-eviction mechanics | Separate grid/loupe planning, retries, cancellation, priorities and budgets |
| R10 | Delivered: renderer-only thresholds, frame bounds and tile rules live in core rendering | Renderer-only policy owned by core rendering; shared adapter admission stays with its actual owner |
| R11 | Delivered: indexing and Locate share one hidden-file predicate and retain separate traversal policies | One hidden-status helper, separate traversal policies |
| R12 | Delivered: comments describe current GPU-first paths and whole-boundary proof helpers are test-only | Current GPU-first contracts and accurate proof-only boundaries |

Registry-aware provider access is verified from outside core, including disabled built-ins and all lookup paths. Exact stream preparation reuses the full Point-sampled, unshaped compilation owned by Evaluation. Plan/compile counters prove each preparation happens once. Matched native setup measurements cover both domains at 24/60 MP. Preview selection/intent and coupled settled/crop resource phases are explicit. Shared layout keeps the budgets and distinct accounting scopes.

## GPU types and ownership

There are two small static workspace boundaries:

- Shared internal GPU types, `luxforge-gpu-types`, for common formats, plane-size/extent rules, pass shapes, shader conventions and resource-layout data/functions. It imports neither core, Iced nor wgpu. Core planning, the backend and desktop lowering can consume it.
- A wgpu backend, `luxforge-gpu`, for shader composition/compilation, execution, common allocation and retirement, and the window-free tile runner. It imports the contract and pinned GPU dependencies, never core or Iced.

The core retains semantic recipe compilation, stage/window planning and the `TileService` contract. The desktop retains host composition, core-to-device lowering, source/evaluation ownership, adapter policy and client/worker scheduling that depends on core requests. Lowering has one owner and is shared by display and tile work. The UI retains Iced device integration, presentation, frame pacing and display-specific scheduling. The UI supplies Iced’s own device to the backend.

Semantic core plans and executable device plans are not identical: lowering resolves light references, runtime words, bindings and geometry. Common primitives are shared while those distinct plan representations remain with their owners. The `PlaneSize::Light` backend distinction stays explicit where required.

There is one executor and one desktop lowering path. The thin Iced presentation adapter borrows the executor’s output; it adds no parallel renderer or compatibility layer. Repository source/dependency checks enforce ownership. Qualification exercises the production kernels.

### Ownership boundary

`luxforge-gpu-types` and `luxforge-gpu` are static workspace crates. Neither exposes an external plugin ABI.

| Owner | Responsibility |
| --- | --- |
| `luxforge-gpu-types` | Common `BoundaryFormat`, `PlaneFormat`, reduced/fixed plane extents, precision/channel compatibility, `PassShape`, words/block headers, pass parameter indices/stride, binding indices, workgroup dimensions and shared WGSL prelude/stubs. Pure resource-layout functions describe scratch grouping, kept/intermediate/output extents, size buckets and parameter slices. No recipe, executable whole plan, device, shader compiler or policy budgets. |
| `luxforge-core` | Module-owned WGSL and semantic descriptions (`render/gpu/program.rs`, `spatial.rs`), recipe compilation, mask sampling, stage/window walk, chained/staged/light-sweep planning, conservative admission and `tiles::TileService`. Semantic planes retain their scratch declarations; a semantic fixed one-texel light is resolved by lowering rather than conflated with a runtime resource reference. |
| `luxforge-gpu` | `gpu_preview` shader assembly, `compile`, `chain`, `spatial`, `source`, `light`, `staged`, `rest` and `histogram` execution/allocation; executable plans, sources, buffers and resource slots; bounded compile/pass caches; GPU availability/loss and resource charge/retirement mechanisms; `tiles::TileRunner` and worker readback. Backend plane sizing uses the common reduced/fixed extent plus an explicit resolved `Light(index)` reference. |
| `luxforge-app` | The sole semantic-to-executable lowering in `app/gpu_plan.rs`, shared by display, tile reads, export and catalog tiers; core-aware evaluation/source lifetimes, stream strategy selection, client cancellation, worker/band scheduling, output encoding installation, adapter selection/refusal policy and complete reference restart. |
| `luxforge-ui` | `PhotoSurface` and Iced `shader::Pipeline` implementation, texture placement/bindings for presentation, crop/overlay/photo presentation and its separate photo-texture charges, redraw/dissolve pacing and UI diagnostics projection. It hands the backend the device/queue supplied by Iced and borrows the backend output view; it does no recipe lowering. |
| Qualification | Backend headless execution/readback helpers and kernel tests live with the executor, behind test-only `qualification`. Core WGSL tests use the common convention. App tests/corpus harness compare the production backend against core/reference and exercise host cancellation/fallback. The independent numerical oracle remains workspace-independent. |

Production dependencies are exactly these workspace edges: `core -> gpu-types`; `gpu -> gpu-types`; `ui -> gpu`; `app -> core + ui + gpu`. The UI may import the pure contract directly only where its presentation needs a common format. The contract uses `std` only. The backend uses the pinned `wgpu = 27.0.1` (`wgsl`, `naga-ir`), `half = 2.7.1` (`std`) and `bytemuck = 1.25.2`. `iced` and `iced_wgpu` stay in UI/app. Test-only dependencies (`luxforge-testbase`, `luxforge-reference`, `luxforge-process`) remain test-only; backend `qualification` enables only its bounded headless wait helper. The CLI reaches core and the pure contract, with no GPU/GUI dependency. Repository rules enforce these transitive boundaries and the qualification/source ownership checks.

The concrete presentation seam is an executor constructed from borrowed/cloned `wgpu::Device`/`Queue`, a neutral wake callback, and explicit device limits/target format. Its per-surface execution state groups the slot, lights, rest accumulator and histogram resources; preparation returns output view/size, readiness/failure and figures. Iced's adapter keeps surface IDs, presentation uniforms, dissolve and redraw decisions. `PhotoPipeline`, `Picture`, `SurfaceSlots`, `SurfaceFigures` and `wake_surface` stay on the UI's side of this seam, and the backend's `compile` takes a neutral wake callback rather than importing from `photo_surface`.

Retirement uses one device-scoped blocking-idle worker, with a 2 ms nonblocking poll only while work is outstanding. Its wgpu completion/retention mechanism lives in the backend; neutral retained resources and completion callbacks allow the UI's photo budget to discharge independently from the preview budget. There is no extra polling worker or early discharge. Presentation keeps its distinct current/retiring-photo admission policy.

Display uses Iced's own device. The tile worker opens its separate worker device on the window's matching backend/name, with the same requested limits and adapter-mismatch refusal; there is no further device. The wgpu enumeration/open mechanism is core-free in the backend, while software-adapter adoption and launch policy remain host choices. Worker waiting and display's nonblocking submission remain separate.

Layout takes boundary origin/size, format, plane roles, link/tail/output requirements and buffer lengths; actual storage offset alignment and texture/binding limits are explicit device inputs. The 256-byte pass stride is checked against supported device alignment. Logical core estimates, actual resident allocation and retiring allocation remain separate. The conservative light allowance stays explicit; the desktop's unconverted estimate is not an admission upper bound. The bounds are the 2 GiB preview budget, 2 GiB tile-worker budget, 256 MiB boundary cap, 256 MiB read reserve, two tiles/two export bands in flight and all cache caps. Crop/overlay/backend staging remain outside the accounting scope; these charges provide no total-memory guarantee.

A consequential boundary change requires owner review and a validated design/DAG.

### Resource layout

`luxforge-gpu-types::layout` owns physical plane classes, kept/scratch/light placement, per-link scratch counts and pool maxima, texture/parameter bytes, tail precision and output/buffer buckets. Core prediction and backend allocation use those rules. Device side/binding limits, fixed-stride alignment and policy ceilings are explicit inputs; whole photo presentation retains its separate tiled filtering aprons. Actual backend admission still guards resource creation.

Keep logical estimates, resident allocations, retiring resources, upload/readback staging and process RSS distinct. Light-buffer allowances must be explicit; do not turn a texture-only estimate into a total-memory guarantee. The desktop's unconverted fallback estimate omits parameter slices and is not an upper bound: it must not replace conservative admission. Total-memory accounting is separate work, and no accounting scope or budget value changes without its own decision and evidence.

### Worker-scoped preparation

PreparedStream shares the evaluation’s exact compilation and builds its source-derived GPU plan once, then derives chained tile grids, staged sweeps and light sweeps from that value. The stream retains immediate caller validation/refusal; worker selection consumes the prepared value, and a staged stream moves the last sweep’s already selected tile list directly into its Drawing. No source-derived plan or tile list is rebuilt for that handoff. A reduced stage, different mask sampling or different draft semantics can legitimately require a different compilation; reuse only when those semantics match.

Preparation lives only as long as the current read/stream. An evaluation retains its source's memory gate, so it must not become desktop state or an unbounded global cache. Preserve cancellation, bounded in-flight bands, device-loss behavior, output order and complete reference restart for an export that fails on the GPU.

## Current module and operation interfaces

Registered `Provider::descriptor` is registry-aware, including disabled modules, and agrees across lookup paths. `Deref` retains raw implementation hooks; explicitly dereferencing to `ToolModule` reads raw metadata without registry overrides.

The supported current authoring interface is `ToolModule` with its descriptor/action/context/processing types, context ownership and placement helpers, and `EffectDescriptor`, `NewLayer` and `LayerUpdate` builders. An external compile/test proves that path. Built-in field-patch authoring remains internal; the developer control proof has an opaque factory. External loading and a general spatial/plugin authoring framework remain later work.

Colour/spatial operation identity includes the exact unit kind, coefficient and stage bits, with explicit lengths for variable sections. Comparison uses no digest and no diagnostic wording. Conservative compiled-mask allocation identity is retained.

## Preview and cache interfaces

`PreviewRequest` carries `PreviewSelection` (whole or prefix of a committed stack or client draft) and `PreviewRenderIntent` (reference, GPU Fit or GPU region). Fit carries required bounds; a region carries its magnification, reduction threshold and optional reference proxy. Prefix analysis retains its validation refusal; GPU draft prefixes refuse explicitly rather than dropping the GPU request. Session ownership is checked first.

Settled and crop resources group their tiles and version identity with pending/ready/refused conversion. Settled picture and counts-only variants become ready together; one presentation decision applies the common gate, gesture, crop and evidence guards before choosing picture or counts. Compare, retained content, source readiness, warm work and counts targets retain independent lifetimes.

Grid and loupe share private `preview_read` protocol/error decoding and `decoded_handles` source-key/side matching, decode/error handoff, one-time handles, replacement/release accounting and atomic protected-entry eviction. Source-key acceptance deliberately keeps a finished decode if a newer plan still wants it. Each controller supplies its own caps, eligible candidates and eviction order. Grid revision/cell planning and supersession remain separate from loupe tier/role selection, abandonment, lookahead and priority. There is no combined controller or general cache framework.

## Small ownership rules

Renderer-only `RenderPass` thresholds, RGBA frame limits and spatial tile/halo rules live in core rendering. RAW source/development admission and genuinely shared adapter bounds stay with their actual owners, and repository rules and imports follow them.

Dot-file/macOS `UF_HIDDEN`/Windows hidden-attribute detection is shared within core. Index and Locate traversal exclusions remain independent.

Comments describe the GPU-first paths, and proof/qualification helpers sit behind their real boundary. Trace any supposedly unused helper before deleting it. Comments must not turn the supported no-GPU fallback into an abandoned path.

## Scope limits and invariants

Preserve the distinct production GPU, whole-frame CPU fallback/reference and independent numerical oracle. Preserve byte/JPEG and linear/RAW quantization and source semantics; reduced-source motion versus process-first settled tiles; chained/staged/light-sweep memory strategies; and nonblocking display versus worker readback pacing. Numerical Rust and WGSL implementations remain separately testable.

The structure leaves source files, recipes, immutable history, command schemas, declared output tolerances, slider responses, GPU budgets, private camera inventory and platform support to their own designs, and adds no UI controls, JSON/MCP operations, dynamic module loading, CLI GPU enablement, generalized node graph, release framework or public compatibility layer. A changed format rejects unsupported data explicitly rather than rewriting it.

## Qualification scope and remaining work

Native M4 full verification passes all 77 components and the complete available authentic-source corpus of 277 recipe/source pairs, with originals unchanged. Matched 30-sample measurements record candidate preparations/recompiles, edit/commit/export-stream costs and charged peaks. Current Linux aarch64 no-adapter headless checks, release owner acceptance/build/enumeration and all 13 software Vulkan/Xvfb journeys pass. Software/VM results are functional evidence only.

Native Windows/Linux GPU qualification is unavailable and remains open; Windows CI is disabled. There is no authentic 60 MP RAW source for desktop/export measurements. Crop/overlay, backend staging and driver/pipeline overhead remain outside the current GPU charges, so complete memory accounting remains open. Those limitations are retained in [performance](../specs/performance.md#code-structure-consolidation) and the [roadmap](../plan.md); consolidation makes no claim for that missing scope.

## Performance review

Provider lookup, current-contract comments, module surface visibility, typed preview reads, renderer policy ownership and hidden metadata sharing add no original read/hash/decode path, frame allocation, render in a query/validation/no-op check, owner-thread frame work, desktop refresh/upload, timer, poll, subscription or cache. Exact rendering/admission and protocol/traversal tests cover them; they involve no image algorithm. The shared preview handle/cache mechanisms retain worker reads, cancellation tokens, bounded handoff capacities and image sizes. Handles keep controller-specific metadata without copying source keys; eviction clones only selected keys after fitting both caps, and refuses without evicting when protected entries prevent a fit. No worker, timer, original read, frame copy or budget is added. Stream preparation shares the bound compilation/source and keeps no cache, worker or pixel copy of its own; strategy selection drops it before drawing begins, while the stream evaluation keeps its request lifetime. The integrated native measurements record photo-sized timings and charge peaks; they make no generalized speed or total-memory claim.

The GPU executor adds no source read/decode, frame rendering on the owner, physical full-frame allocation or timer. The UI binds output textures and placement buffers through one bounded presentation wrapper per gesture/rest slot; texture identity reuses the binding and uniform-write cache. One retirement worker blocks idle and polls every 2 ms only while work is outstanding; photo and preview charges remain independent through their completion fence. Native rendered regressions cover pixel equivalence, histogram counts, scratch sharing, refits, warm/cancelled compiles, budgets, retirement and fallback. The integrated 24/60 MP measurements retain the workload/cache/adapter/load scope; no generalized speed or total-memory claim is made.

## Acceptance and verification

Delivered outcomes are one registry availability answer; common GPU format/extent/convention and layout definitions checked against actual allocations; one production executor and one lowering path; one equivalent worker stream preparation; supported module builders/context helpers; exact operation identity; explicit preview intent and resource phases; shared preview/cache mechanics with controller-specific policy; and renderer/hidden-metadata ownership.

Exact reference tests, native GPU tolerance comparisons, original/provider recovery, cancellation, budgets, retirement and device-loss tests pass. Background rendered journeys correlate preview, Compare/crop, histogram, export, grid/loupe and RAW state with logs/captures. The native full run uses the authentic-source manifest and fresh output; current Linux functional results retain their software/no-adapter scope. [Performance](../specs/performance.md#code-structure-consolidation) records complete evidence, sample counts and missing scope.

The neutral tile runner accesses the executor’s private shader, source, scratch and chain implementation directly. Desktop clients call the backend directly; graphics enumeration/device opening is neutral, while renderer selection, software/refusal policy and requested limits remain in the desktop. Display keeps Iced’s device and its nonblocking pacing; the tile worker keeps its separate device and bounded readback.

The [architecture](architecture.md), [module contract](modules-and-api.md), [GPU-first contract](gpu-first.md), [feature status](../features.md) and performance specification describe the delivered shape.
