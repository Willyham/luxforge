# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

## Engineering plans

| Plan | Purpose |
| --- | --- |
| [Rendering](rendering.json) | Measure and bound the GPU resources outside the photo-texture ceiling |
| [RAW](raw.json) | The RAW qualification milestone: controlled quality, the foundation and journey checkpoints, failure hardening, packaging and whole-editor measurement |
| [Tone curve](tone-curve.json) | The Tone curve module: frozen curve numerics, the shared curve-editor changes, the field-patch module, conformance and placement, the desktop scenario, presets, masks, harness preparation and measurement |
| [Detail](detail.json) | Capture sharpening and manual noise reduction in implementation: the restoration stage and compile context, off-owner pixel queries and mutations, the 16-bit JPEG hand-off, exact settled Fit, the restoration-prefix proxy cache, the input-grid overlay cache, shared controls/masks/presets/history and quality/performance qualification |
| [Efficiency](efficiency.json) | Byte-identical CPU and memory reductions: hardware SHA-256 and build features, source preparation, the 16-bit quantizer, spatial tile buffers and batches, Detail and Presence kernels, the reduced-grid cache, RAW row reads, the float mosaic through a checked-in librtprocess patch, owner and painting copies, the WAL catalog and desktop derivation, measured once at the end; the `dist` profile is deferred |
| [GPU-first rendering](gpu-first.json) | The GPU as the renderer of record for the picture, the histogram, samples and export, with correctness a declared tolerance against a whole-frame CPU reference: every zoom on the GPU, the picture at rest and the histogram on the GPU, per-frame estimates, samples and export through GPU tiles, portability and warm-up, the retirement of the CPU production paths, and one qualification and measurement at the end |
| [Lens and perspective](lens-and-perspective.json) | Implemented and functionally verified offline profile distortion and manual perspective; native performance and photographic qualification pending: the pinned Lensfun resource index, the optics ledger, the mapping refactor and warp chain with closed-form coverage, the query-choice control, coordinate preservation and bounded verification |

The post-consolidation programme is complete and its plans are deleted; its outcome lives in the specs and [feature status](../docs/features.md).

Detail's choices are delegated and recorded in its [design](../docs/design/detail.md), with the owner's four decisions of 2026-09-30 (the 16-bit JPEG hand-off, the restoration-prefix proxy cache, the input-grid overlay cache and the off-owner mutation path) in [decisions](../docs/decisions.md#detail). The plan has no owner-review prerequisite; implementation is authorized and in progress.

The Lens and perspective plan runs on the owner's decisions of 2026-09-30 in [decisions](../docs/decisions.md#lens-and-perspective-planning) and the recorded defaults in its [design](../docs/design/lens-and-perspective.md); functional implementation and native integration are complete. Its measurement task waits for the other feature work and a quiet host. Profile accuracy qualification needs authentic photographs and marked edges the owner supplies; without them it stays incomplete.

The Tone curve's features are implemented (authorized by the owner on 2026-09-30); only the measurement task, TASK-009's point-drag and unit-cost figures, remains. The module runs on the owner's below-black decision of 2026-09-30 and the recorded defaults in the [Tone curve design](../docs/design/tone-curve.md#proposals-with-recorded-defaults), each a proposal the owner can revise.

The Efficiency plan runs on the owner's decisions of 2026-10-03 in [decisions](../docs/decisions.md#cpu-and-memory-efficiency). Every feature task is done and measured within the three hours the owner allotted to measurement on 2026-10-04 ([performance](../docs/specs/performance.md#cpu-and-memory-efficiency-measured-on-the-m4)); each change paid for itself where measured, and the workloads not measured are listed there. Its one open task is the `dist` profile, blocked: the owner deferred it until the other plans' outstanding timing runs are recorded, so every plan measures in `release`.

The GPU-first plan runs on the owner's decision of 2026-10-04 in [decisions](../docs/decisions.md#gpu-first-rendering) and the recorded defaults in its [design](../docs/design/gpu-first.md); the remaining questions are a [product decisions](product-decisions.json) task. It supersedes the Detail plan's measurement of the settled Fit and the prefix cache, which is cancelled, and changes the latency baselines the Tone curve and Lens measurements record; each of those tasks notes it. The rendering plan's GPU accounting is a prerequisite of any total-memory claim it makes.

The three modules share the restoration stage, compile context, window planner, mask input mapping and Fit settlement contracts, with combined exact-buffer and native rendering checks. Their outstanding measurements run after all feature work, one plan at a time on a quiet host.

## Other plans

| Plan | Purpose |
| --- | --- |
| [Corrections](corrections.json) | Proposed offline Clone/Heal, the repair stage, the shared brush and the repair layer's frozen-patch operation; its AI Remove is planned under AI editing |
| [AI editing](ai-editing.json) | Proposed Remove, Select, generative fill and Replace, and optional sky replacement on local, user-downloaded models: the inference port and runtime, the model manager, the analysis cache and picker, the model-selection mask kind, the fill tiers, the remote-provider shape, portability, qualification and documentation |
| [Dependency advisories](dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Product decisions](product-decisions.json) | Open product questions |

The Corrections plan covers Clone and Heal, the repair stage, the shared brush and the frozen-patch operation; its provider qualification, local and remote adapters and AI Remove workflow are cancelled there and carried by the AI editing plan, which depends by name on that foundation. The owner decided the repair stage, the deletion of `PointReplace` and the stale-patch rule on 2026-10-05 ([decisions](../docs/decisions.md#ai-editing)); its first task now freezes the Clone/Heal contract on those decisions.

The AI editing plan runs on the owner's decisions of 2026-10-05 in [decisions](../docs/decisions.md#ai-editing): quality-first model choice with no licence or provenance gate, ONNX Runtime, stale patches kept rendering, budgets raised for quality, consent remembered per provider, sky replacement in scope. It builds on the object removal research, the standalone prototype measured on the M4 (on the `claude/object-removal-research-d3374b` branch until its harness task brings it onto `main`), the implemented module capabilities and the delivered mask model. The decisions task is complete; the harness task and the two foundation tasks that enter `crates/` are ready, the qualification tasks need only the harness, and the plan is sequenced after the GPU-first integration branch merges, with the Corrections foundation beside its first stage.

UI themes are implemented and their plan is deleted; the outcome is in the [UI themes design](../docs/design/ui-themes.md) and [feature status](../docs/features.md), and the remaining questions are a [product decisions](product-decisions.json) task.

RAW editing is implemented with initial native M4 verification. Its [design](../docs/design/initial-raw.md) records the owner's continuous RAW editing requirement, the pinned processing path, and outstanding controlled quality, resource and platform qualification, which the RAW plan carries. The supplied FC3411 DNG has required gain/warp corrections and continuous editor support; its [contract](../docs/design/air2s-dng.md) records the qualified encoding, numerical interpretation and limits. RAW recipes export through the shared [JPEG exporter](../docs/design/export.md).

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- A task's `test_strategy.commands` are its acceptance checks, run once when the task is complete, not after each step. Put timing commands in the task that completes the feature, or in a task whose subject is performance, so a plan measures once.
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links do not point into another task plan. The advisory audit reads the dependency advisories plan to confirm each exception's task remains open.
