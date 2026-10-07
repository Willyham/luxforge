# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

## Planning for execution

A plan handed off for implementation is executable work. Resolve material product questions while creating or revising it; the task list must not send the implementer back to ask what to build.

- Reuse existing context, decisions and authorization. Ask only questions whose answers materially change the product or scope, bundle them where possible, and resolve them before handing off the affected implementation. Routine numerical, layout and engineering choices belong to the implementer within the recorded contract; state that delegation instead of adding a review task.
- Give every task a concrete output, sufficient context and observable acceptance. Do not add tasks to ask the owner, obtain implementation approval, confirm scope or freeze a contract through another decision round. Record decisions in the design and [decisions](../docs/decisions.md), not as future implementation work. A genuine discovery task produces an artifact or capability consumed downstream, with a reason it must come first.
- Maximise work that can finish autonomously. Build a useful working feature first where possible. Put owner review, Lightroom/reference matching, calibration, extensive qualification and other refinement after that delivery unless their output is genuinely needed to build it correctly. Ordinary correctness, source-preservation and rendered integration checks stay with the code they prove; moving perfection later does not make an unrun check a pass.
- Dependencies are required outputs, not ceremonial phase gates. Independent work can run together; late review or refinement has no dependency back into the working-feature handoff. Name any unavoidable prerequisite and its concrete effect on the consuming task.
- Identify external inputs while planning. Split data/tooling preparation and independent implementation from the part that needs those inputs; do not make a whole feature wait for a manual export, private data or final approval. Use honest measured/unmeasured status where inputs are absent. If a final consequential action needs approval, prepare its concrete reviewable result first.

A planning request still does not authorize executing the implementation or unrelated external actions. Preserve actual authorization boundaries, settle needed authorization during planning rather than adding an approval task, and distinguish an unfinished proposal from an implementation-ready plan. When revising existing plans, preserve IDs, truthful statuses and completed work; apply these conventions to the remaining work without silently changing scope or prior decisions.

## Layout

Plans live in one folder per area. A plan moves only when its area changes; its status lives in its tasks, not in its location. Plans that are validated and have tasks ready to implement are listed in the roadmap's [ready to implement](../docs/plan.md#ready-to-implement) section, with the minimum model and the re-review every task needs before it starts.

### Rendering (`rendering/`)

The renderer, its GPU and CPU paths, memory and efficiency.

| Plan | Purpose |
| --- | --- |
| [Efficiency](rendering/efficiency.json) | Reference-renderer, preparation and desktop CPU/memory reductions: hardware SHA-256 and build features, source preparation, the 16-bit quantizer, spatial tile buffers and batches, Detail and Presence kernels, the reduced-grid cache retired with the CPU production paths, RAW row reads, the float mosaic through a checked-in librtprocess patch, owner and painting copies, the WAL catalog and desktop derivation, measured once at the end; the `dist` profile is deferred |
| [GPU-first rendering](rendering/gpu-first.json) | The GPU as the renderer of record for the picture, the histogram, samples and export, with correctness a declared tolerance against a whole-frame CPU reference: every zoom on the GPU, the picture at rest and the histogram on the GPU, per-frame estimates, samples and export through GPU tiles, portability and warm-up, the retirement of the CPU production paths, and one qualification and measurement at the end |
| [GPU memory accounting](rendering/gpu-memory.json) | Measure and bound both GPU devices, including resources outside the 1 GiB aggregate photo slots and separate 2 GiB preview and tile-worker budgets, with resources.read and Performance attribution |

### RAW (`raw/`)

RAW development and looks.

| Plan | Purpose |
| --- | --- |
| [RAW looks](raw/raw-looks.json) | New RAW photos start from a Luxforge look: the frozen look units and the corpus-chosen Standard look in the Original with its preference and section (phase 1), then Match camera fitted to the embedded preview off the owner (phase 2), measured once at the end |

### Editing tools (`editing/`)

One plan per tool module.

| Plan | Purpose |
| --- | --- |
| [AI editing](editing/ai-editing.json) | Proposed Remove, Select, generative fill and Replace, and optional sky replacement on local, user-downloaded models: the inference port and runtime, the model manager, the analysis cache and picker, the model-selection mask kind, the fill tiers, the remote-provider shape, portability, qualification and documentation |
| [Corrections](editing/corrections.json) | Proposed offline Clone/Heal, the repair stage, the shared brush and the repair layer's frozen-patch operation; its AI Remove is planned under AI editing |
| [Colour grading](editing/colour-grading.json) | Decided, implementation-ready mixer extension: tonal/Global wheels, masks, presets and direct Lightroom mappings delivered first; independent reference analysis and Lightroom response refinement last, with no approval/research gates |

### Lightroom (`lightroom/`)

Import from and alignment with Lightroom Classic.

| Plan | Purpose |
| --- | --- |
| [Lightroom alignment](lightroom/lightroom-alignment.json) | Decided, not authorized: the measurement rig over deterministic reference-rendered study outputs, two Lightroom rounds by the owner, calibrated conversions for the importers with RAW preset white balance in its round, each control's response realigned by editing area with its CPU reference and GPU program, targeted behaviour changes past the threshold, and validation on the owner's real edits |
| [Lightroom import](lightroom/lightroom-import.json) | Decided, not authorized: a read-only import of Lightroom Classic catalogs and sidecar folders, the photographs worked on with their organization, settings, report, re-mapping and Lightroom's first grid tiles only, with no import undo (phase 1), masks (phase 2), and mappings that land with later capabilities or alignment measurements (phase 3) |

### Interface (`interface/`)

| Plan | Purpose |
| --- | --- |
| [High-zoom minimap](interface/minimap.json) ([design](../docs/design/minimap.md)) | Planned whole-image overview at percentage zoom ≥200, with owner-chosen click/drag navigation through `view.set`, bounded GPU/reference rendering, working integration first and final qualification/measurement; implementation awaits authorization |

### Project (`project/`)

Owner decisions and repository upkeep.

| Plan | Purpose |
| --- | --- |
| [Dependency advisories](project/dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Live-session Rust CLI](project/live-cli.json) ([design](../docs/design/live-cli.md)) | Specified thin client for the authenticated API of an already-open desktop session; implementation awaits owner authorization |
| [Product decisions](project/product-decisions.json) | Open product questions |

The post-consolidation programme, the Tone curve, Detail and Lens and perspective are complete and their plans are deleted; their outcome lives in the specs, their designs and [feature status](../docs/features.md).

The Efficiency plan runs on the owner's decisions of 2026-10-03 in [decisions](../docs/decisions.md#cpu-and-memory-efficiency). Every feature task is done and measured within the three hours the owner allotted to measurement on 2026-10-04 ([performance](../docs/specs/performance.md#cpu-and-memory-efficiency-measured-on-the-m4)); each change paid for itself where measured, and the workloads not measured are listed there. Its one open task is the `dist` profile, blocked: the owner deferred it until the other plans' outstanding timing runs are recorded, so every plan measures in `release`.

The GPU-first plan runs on the owner's decision of 2026-10-04 in [decisions](../docs/decisions.md#gpu-first-rendering) and the accepted defaults in its [design](../docs/design/gpu-first.md). Its portability task remains open: prior hosted lavapipe journeys miss the large24 deadline and latest Linux CI fails earlier in the indexed-folder watcher test. GPU accounting is a prerequisite of any total-memory claim. Current tolerances and behaviour defaults are accepted by the owner on 2026-10-07; softer budget-reduced motion at 100% still needs explicit qualification and neutral-picker answers still need renderer identity.

The Corrections plan covers independently delivered and qualified offline Clone/Heal, the repair stage and production shared-brush interaction, plus the same repair layer's frozen-patch and bounded candidate foundation. It reuses the implemented generic path/stroke/draft plumbing, current modules/capabilities, Develop/prepare/adopt, catalog cleanup and nonlinear geometry; renderer integration targets the GPU renderer of record, required corpus qualification, tile-service reads/export, windows, staged sweeps, incremental chains and warm-up. Provider qualification, inference, adapters and the AI Remove workflow remain cancelled here and owned by AI editing, which depends by name on the foundation. The owner decided repair after source development and before colour, deletion of `PointReplace` and stale patches kept rendering on 2026-10-05 ([decisions](../docs/decisions.md#ai-editing)); the first task freezes the remaining contract, preserving the owner's 2026-10-07 decisions: repair before Detail with rerun cost measured, the spatial qualification class and exclusion from presets. Frozen-patch conformance belongs to its own task and does not block offline Clone/Heal qualification.

The AI editing plan runs on the owner's decisions of 2026-10-05 in [decisions](../docs/decisions.md#ai-editing): quality-first model choice with no licence or provenance gate, ONNX Runtime, stale patches kept rendering, budgets raised for quality, consent remembered per provider, sky replacement in scope. It builds on the object removal research, the standalone prototype measured on the M4 (on the `claude/object-removal-research-d3374b` branch until its harness task brings it onto `main`), the implemented module capabilities and the delivered mask model. The decisions task is complete; the harness task and the two foundation tasks that enter `crates/` are ready, the qualification tasks need only the harness, and the plan integrates with the merged GPU-first interfaces, with the Corrections foundation beside its first stage. The owner decided reference inputs for all AI work, locking the generating photo while another stays editable, and combined renderer/inference memory qualification on 2026-10-07.

The RAW looks plan runs on the owner's decisions of 2026-10-05 in [decisions](../docs/decisions.md#raw-looks), including its design's defaults, which the owner accepted the same day. Phase 1 (the Standard look) is built and the owner-approved numbers are frozen; supplied-file journeys pass, while the rendered tier with the owner manifest and the Neutral Amount control remain outstanding. Match camera remains planned. GPU-first is merged; its fused look program is registered and device-qualified. The corpus release gate covers Standard at amount 100 implicitly in RAW Originals; Neutral, other amounts and Camera fits are not separately qualified on photographs by that gate.

The Lightroom import plan runs on the owner's decisions of 2026-10-06 in [decisions](../docs/decisions.md#lightroom-import). Implementation is not authorized; its first task, confirming the catalog format against a copy of the owner's catalog, needs no code.

The Lightroom alignment plan runs on the owner's decisions of 2026-10-06 in [decisions](../docs/decisions.md#lightroom-alignment). Implementation is not authorized. Its rounds need the owner to import and export in Lightroom; its response changes raise each module's format marker, so earlier recipes are refused.

UI themes are implemented and their plan is deleted; the outcome is in the [UI themes design](../docs/design/ui-themes.md) and [feature status](../docs/features.md), and the remaining questions are a [product decisions](project/product-decisions.json) task.

RAW editing is implemented with initial native M4 verification. Its [design](../docs/design/initial-raw.md) records the owner's continuous RAW editing requirement, the pinned processing path, and the controlled quality, resource and platform qualification the owner closed unrun on 2026-10-06. The supplied FC3411 DNG has required gain/warp corrections and continuous editor support; its [contract](../docs/design/air2s-dng.md) records the qualified encoding, numerical interpretation and limits. RAW recipes export through the shared [JPEG exporter](../docs/design/export.md).

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- A task's `test_strategy.commands` are its acceptance checks, run once when the task is complete, not after each step. Put timing commands in the task that completes the feature, or in a task whose subject is performance, so a plan measures once.
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- A new plan goes in its area folder. When a validated plan gains a `ready` task, or its last ready task starts or finishes, update the roadmap's [ready to implement](../docs/plan.md#ready-to-implement) list in the same change.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory and its area folders against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links stay in the repository (no absolute path, no `..` out of it) and do not point into another task plan. The advisory audit reads the dependency advisories plan to confirm each exception's task remains open.
