# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

## Layout

Plans live in one folder per area. A plan moves only when its area changes; its status lives in its tasks, not in its location. Plans that are validated and have tasks ready to implement are listed in the roadmap's [ready to implement](../docs/plan.md#ready-to-implement) section, with the minimum model and the re-review every task needs before it starts.

### Rendering (`rendering/`)

The renderer, its GPU and CPU paths, memory and efficiency.

| Plan | Purpose |
| --- | --- |
| [Efficiency](rendering/efficiency.json) | Byte-identical CPU and memory reductions: hardware SHA-256 and build features, source preparation, the 16-bit quantizer, spatial tile buffers and batches, Detail and Presence kernels, the reduced-grid cache, RAW row reads, the float mosaic through a checked-in librtprocess patch, owner and painting copies, the WAL catalog and desktop derivation, measured once at the end; the `dist` profile is deferred |
| [GPU-first rendering](rendering/gpu-first.json) | The GPU as the renderer of record for the picture, the histogram, samples and export, with correctness a declared tolerance against a whole-frame CPU reference: every zoom on the GPU, the picture at rest and the histogram on the GPU, per-frame estimates, samples and export through GPU tiles, portability and warm-up, the retirement of the CPU production paths, and one qualification and measurement at the end |
| [GPU memory accounting](rendering/gpu-memory.json) | Measure and bound the GPU resources outside the photo-texture ceiling |

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

### Lightroom (`lightroom/`)

Import from and alignment with Lightroom Classic.

| Plan | Purpose |
| --- | --- |
| [Lightroom alignment](lightroom/lightroom-alignment.json) | Decided, not authorized: the measurement rig over rendered outputs, two Lightroom rounds by the owner, calibrated conversions for the importers, each control's response realigned by editing area, targeted behaviour changes past the threshold, and validation on the owner's real edits |
| [Lightroom import](lightroom/lightroom-import.json) | Decided, not authorized: a read-only import of Lightroom Classic catalogs and sidecar folders, the photographs worked on with their organization, settings, report, re-mapping and Lightroom's previews as first tiles (phase 1), masks (phase 2), and mappings that land with later tools (phase 3) |

### Project (`project/`)

Owner decisions and repository upkeep.

| Plan | Purpose |
| --- | --- |
| [Dependency advisories](project/dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Product decisions](project/product-decisions.json) | Open product questions |

The post-consolidation programme, the Tone curve, Detail and Lens and perspective are complete and their plans are deleted; their outcome lives in the specs, their designs and [feature status](../docs/features.md).

The Efficiency plan runs on the owner's decisions of 2026-10-03 in [decisions](../docs/decisions.md#cpu-and-memory-efficiency). Every feature task is done and measured within the three hours the owner allotted to measurement on 2026-10-04 ([performance](../docs/specs/performance.md#cpu-and-memory-efficiency-measured-on-the-m4)); each change paid for itself where measured, and the workloads not measured are listed there. Its one open task is the `dist` profile, blocked: the owner deferred it until the other plans' outstanding timing runs are recorded, so every plan measures in `release`.

The GPU-first plan runs on the owner's decision of 2026-10-04 in [decisions](../docs/decisions.md#gpu-first-rendering) and the recorded defaults in its [design](../docs/design/gpu-first.md); the remaining questions are a [product decisions](project/product-decisions.json) task. The rendering plan's GPU accounting is a prerequisite of any total-memory claim it makes.

The Corrections plan covers independently delivered and qualified offline Clone/Heal, the repair stage and production shared-brush interaction, plus the same repair layer's frozen-patch and bounded candidate foundation. It reuses the implemented generic path/stroke/draft plumbing, current modules/capabilities, Develop/prepare/adopt, catalog cleanup and nonlinear geometry; renderer integration targets the merged GPU-first interfaces and comparison harness. Provider qualification, inference, adapters and the AI Remove workflow remain cancelled here and owned by AI editing, which depends by name on the foundation. The owner decided repair after source development and before colour, deletion of `PointReplace` and stale patches kept rendering on 2026-10-05 ([decisions](../docs/decisions.md#ai-editing)); the first task freezes the remaining contract, including the open repair-versus-Detail placement. Frozen-patch conformance belongs to its own task and does not block offline Clone/Heal qualification.

The AI editing plan runs on the owner's decisions of 2026-10-05 in [decisions](../docs/decisions.md#ai-editing): quality-first model choice with no licence or provenance gate, ONNX Runtime, stale patches kept rendering, budgets raised for quality, consent remembered per provider, sky replacement in scope. It builds on the object removal research, the standalone prototype measured on the M4 (on the `claude/object-removal-research-d3374b` branch until its harness task brings it onto `main`), the implemented module capabilities and the delivered mask model. The decisions task is complete; the harness task and the two foundation tasks that enter `crates/` are ready, the qualification tasks need only the harness, and the plan is sequenced after the GPU-first integration branch merges, with the Corrections foundation beside its first stage.

The RAW looks plan runs on the owner's decisions of 2026-10-05 in [decisions](../docs/decisions.md#raw-looks), including its design's defaults, which the owner accepted the same day. Phase 1 (the Standard look) is built first and is deliverable alone; its first task freezes the Standard's numbers only after the owner reviews a contact sheet. It shares the GPU program list and qualification steps with the GPU-first integration, regenerated after either lands.

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

`cargo xtask check` validates every JSON file in this directory and its area folders against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links do not point into another task plan. The advisory audit reads the dependency advisories plan to confirm each exception's task remains open.
