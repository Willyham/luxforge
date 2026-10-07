# Code structure consolidation

Status: planned. This design and its [task plan](../../tasks/project/code-structure.json) turn the code-structure review into bounded work. Implementation is not authorized by the planning request. The GPU crate split below is a proposal; its boundary task resolves the consequential choices before extraction begins.

## Objective

Remove repeated contracts and redundant preparation, make current Rust interfaces coherent, and separate reusable GPU execution from presentation. Preserve the current image, command, catalog, failure and scheduling behavior. Success is fewer independent definitions and fewer places rebuilding invariant-sensitive rules, rather than a lower file or line count.

The core continues to own transactions, history, validation and bounded services. Modules continue to declare semantic operations. GPU correctness remains the declared tolerance against the whole-frame CPU reference, with the independent numerical reference isolated from production code. Current formats only; no compatibility adapters, migrations, plugin ABI or generalized render graph.

## Current structure and findings

The dependency graph already has valuable boundaries: the core has no production GPU or GUI dependency, widgets do not import core, the JSON CLI builds without the GUI/GPU stack, and the numerical reference cannot import a workspace crate. Host parameter declarations generate parsing and schemas together; the method table owns dispatch; field-patch modules share validation and planning; the rendering pipeline shares control flow through its pixel domain; and `Latest` provides a shared bounded worker primitive. Preserve these boundaries and mechanisms.

The current GPU path is core compilation/planning, desktop lowering, and execution under the UI's photo surface. The window-free `TileRunner` shares the production pipeline with display, while the desktop's tile worker adapts it to the core's `TileService`. Export, samples and analysis now use this execution as well as display.

| Finding | Current issue | Outcome | Tasks |
| --- | --- | --- | --- |
| R1 | `Provider::descriptor` is crate-private while public `Deref` exposes the module's descriptor; an external caller can read the module's available status instead of the registry override | One registry-aware availability answer through the public lookup surface | TASK-001 |
| R2 | Core and UI separately declare GPU formats, plane extents, pass shapes and shader constants; desktop lowering maps the corresponding families | One shared primitive contract; semantic planning and device lowering keep their distinct responsibilities | TASK-003, 004 |
| R3 | Core admission and backend allocation separately calculate common resource-layout rules; tests compare the two implementations | Shared layout rules consumed by prediction and allocation, with explicit device inputs | TASK-005 |
| R4 | Window-free GPU execution is reusable but lives in the widget subsystem | One core-free, Iced-free executor with a thin presentation adapter | TASK-006, 007 |
| R5 | Public module-authoring types expose traits without accessible builders/readers; context helpers are internal while raw fields are public | A coherent current cross-crate surface, with internal authoring machinery clearly internal | TASK-009 |
| R6 | Stream strategy and tile-size selection repeatedly prepare the same GPU stack, and staged selection discards a newly generated tile list | One preparation per equivalent worker-scoped request; derive alternatives and reuse sweep tiles | TASK-008 |
| R7 | Colour and spatial operation equality uses diagnostic description strings | Explicit exact semantic identity independent of diagnostic wording | TASK-010 |
| R8 | Preview requests and resource phases use loose flags/options and repeated eligibility checks | Valid request intent and small resource-phase types; one presentation/counts eligibility decision | TASK-011, 012 |
| R9 | Grid and loupe repeat preview-read parsing and bounded decode/handle-cache mechanics | Shared mechanisms with separate grid/loupe policies and budgets | TASK-013, 014 |
| R10 | RAW exports renderer-only thresholds, frame bounds and tile rules unused by its implementation | Renderer-only policy owned by core rendering; shared adapter admission stays with its actual owner | TASK-015 |
| R11 | Indexing and Locate duplicate hidden-file platform metadata checks | One hidden-status helper, separate traversal policies | TASK-016 |
| R12 | Comments and broad dead-code expectations still describe the earlier gesture-only GPU stage | Current GPU-first contracts and accurate proof-only boundaries | TASK-002 |

R1 is a public API defect: Rust skips the inaccessible inherent method outside core and resolves through `Deref`. Internal core checks read the override; the review did not establish that the editor executes disabled providers. R6 is confirmed redundant construction, not a measured speedup. R7 and R8 are interface risks; no wrong delivered equality or rendering failure was established. R3 is duplicated ownership, not evidence of a budget overrun. The relevant source entry points are linked in each task.

## GPU contract and ownership proposal

Propose two small static workspace boundaries, with names settled by TASK-003:

- A pure internal GPU contract, tentatively `luxforge-gpu-contract`, for common formats, plane-size/extent rules, pass shapes, shader conventions and resource-layout data/functions. It imports neither core, Iced nor wgpu. Core planning, the backend and desktop lowering can consume it.
- A wgpu backend, tentatively `luxforge-gpu`, for shader composition/compilation, execution, common allocation and retirement, and the window-free tile runner. It imports the contract and pinned GPU dependencies, never core or Iced.

The core retains semantic recipe compilation, stage/window planning and the `TileService` contract. The desktop retains host composition, core-to-device lowering, source/evaluation ownership, adapter policy and client/worker scheduling that depends on core requests. Lowering has one owner and is shared by display and tile work. The UI retains Iced device integration, presentation, frame pacing and display-specific scheduling. Share the device supplied by Iced rather than introducing a second device merely to separate crates.

Semantic core plans and executable device plans are not identical: lowering resolves light references, runtime words, bindings and geometry. Share genuinely common primitives instead of forcing both whole plan types into one representation. The existing `PlaneSize::Light` backend distinction remains explicit where required.

Each extraction moves implementation and updates its callers in the same task. Do not keep a parallel old executor or compatibility layer. A thin current presentation adapter is a supported boundary, not a historical interface. Repository source/dependency checks move with ownership and continue to refuse forbidden dependencies. Qualification must exercise the same production kernels after the move.

### Resource layout

Share format sizes, extent rounding, compatible scratch grouping, kept/intermediate/output allocation rules, size buckets and common parameter layout. Device alignment/limits are explicit inputs where needed. Prediction and allocation use the shared result or rules; actual backend admission still guards device resource creation.

Keep logical estimates, resident allocations, retiring resources, upload/readback staging and process RSS distinct. Existing light-buffer allowances must be explicit; do not turn a texture-only estimate into a total-memory guarantee. The desktop's unconverted fallback estimate omits parameter slices and is not an upper bound: it must not replace conservative admission. Existing total-memory work remains separate, and no accounting scope or budget value changes without its own decision and evidence.

### Worker-scoped preparation

Prepare the compilation and source-derived GPU plan once for an equivalent stream request, then derive chained tile grids, staged sweeps and light sweeps from that value. Reuse the chosen sweep's tiles directly. A reduced stage, different mask sampling or different draft semantics can legitimately require a different compilation; reuse only when those semantics match.

Preparation lives only as long as the current read/stream. An evaluation retains its source's memory gate, so it must not become desktop state or an unbounded global cache. Preserve cancellation, bounded in-flight bands, device-loss behavior, output order and complete reference restart for an export that fails on the GPU.

## Current module and operation interfaces

Make the registry-aware provider descriptor public and test it from outside core. Assess `Deref` deliberately: explicit access to raw hooks must not obscure which descriptor carries registry availability. Every public lookup should clearly state whether it returns a registered provider or raw module implementation.

Audit actual cross-crate producers and consumers before changing visibility. Field-patch built-in machinery that has no current external author belongs behind `pub(crate)`; public inputs/outputs used by consumers remain public. Where an existing supported authoring path needs builders, value readers or context invariants, provide a complete small interface and prove it through an external compile/test. External loading and a general plugin authoring framework remain later work; discovering a new authoring requirement does not authorize building it.

Give colour/spatial units an exact semantic identity separate from `describe`. Identity includes the unit kind and every coefficient or stage-dependent value that affects processing. Equality must not depend on a lossy hash. Preserve current conservative mask allocation identity unless separately justified. Diagnostic wording and formatting must be free to change without changing operation equality.

## Preview and cache interfaces

Represent preview selection (committed, draft or prefix) and render/view intent explicitly. A GPU intent carries its Fit/region view and relevant reduction settings. CPU/reference/proxy intent carries only its applicable fields. Preserve current valid requests and errors; no user-visible quality or fallback policy changes. Public construction should reject or make unrepresentable combinations that currently lose intent silently.

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

1. **GPU crate boundary:** adopt the proposed pure-contract plus Iced-free backend split, including actual names and the minimal surface for Iced's device integration. TASK-003 produces the dependency map and exact ownership table, and records the owner's decision before extraction proceeds. If the owner rejects new crates, revise the design and task DAG before implementation; duplicated contract/layout rules still need a single owner.
2. **Current Rust authoring surface:** default to supported current consumers and keep built-in-only machinery internal. TASK-009 inventories usage. If this exposes an actual requirement for new external authors, take that scope change to the owner rather than silently turning a visibility cleanup into a plugin framework.
3. **Implementation authorization:** the owner requested planning and merging those files. The ready tasks remain in the roadmap's awaiting-authorization section until the owner starts this programme.

## Acceptance and verification

Every task has observable acceptance and scoped tests in the JSON plan. The primary outcomes are: one registry availability answer; one common GPU format/extent/convention definition; common layout rules checked against actual allocations; one production executor; one equivalent stream preparation; clear supported module construction/read interfaces; explicit preview intent; and shared cache mechanics with preserved policy.

While implementing, run only the affected scoped tests. Run `quick` once at each task handoff and rendered checks at GPU/UI integration points, using the compiler-cache wrapper on macOS/Linux and background editor launches. Update task check commands when tests move to an actually created package; proposed package names are not commands that exist today. Every output directory is fresh.

The integrated qualification task runs after feature work: native M4 comparison within the existing tolerances for display, histogram, samples, catalog tiers and export; exact CPU/reference regression checks; source preservation and unavailable-provider recovery; correlated state/logs/captures for preview, Compare/crop, grid and loupe; cancellation, budget refusal, retirement and device-loss checks; and existing portability/no-GPU checks. An incomplete or software-adapter run is recorded with its scope, never treated as native evidence.

Measure once on the finished branch against a baseline built separately from the pre-refactor revision: stream setup, representative 24/60 MP commit/edit/export behavior and relevant peak allocations. Use existing harnesses where possible, the specified sample counts, a quiet host and the performance rules. Compile/plan counters demonstrate eliminated preparation; no numerical speed or total-memory claim is made without measurement. Full verification with the authentic-source manifest is required before programme completion; unavailable private fixtures or native hosts leave the corresponding acceptance open.

Finally update architecture/module/rendering documents and feature status to the delivered shape. Update the user guide only if documented behavior or an interface explanation actually changes. Delete the completed task plan and its roadmap/index entries under the repository conventions; this design retains only the current outcome and outstanding decisions.
