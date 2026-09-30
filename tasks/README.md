# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

## Engineering plans

| Plan | Purpose |
| --- | --- |
| [Rendering](rendering.json) | Measure and bound the GPU resources outside the photo-texture ceiling |
| [RAW](raw.json) | The RAW qualification milestone: controlled quality, the foundation and journey checkpoints, failure hardening, packaging and whole-editor measurement |
| [Tone curve](tone-curve.json) | The Tone curve module: frozen curve numerics, the field-patch module, conformance and placement, the desktop scenario, presets, masks and measurement |
| [Detail](detail.json) | Planned capture sharpening and manual noise reduction: pre-tone placement, exact settled Fit, shared controls/masks/presets/history and quality/performance qualification |

The post-consolidation programme is complete and its plans are deleted; its outcome lives in the specs and [feature status](../docs/features.md).

Detail's choices are delegated and recorded in its [design](../docs/design/detail.md). The plan is ready for later implementation without an owner-review prerequisite; the planning request does not itself authorize implementation.

The Tone curve plan is planned work, not yet authorized for implementation: it runs on the recorded defaults in the [Tone curve design](../docs/design/tone-curve.md#proposals-with-recorded-defaults), each a proposal the owner can revise.

## Other plans

| Plan | Purpose |
| --- | --- |
| [Corrections](corrections.json) | Proposed offline Clone/Heal and optional provider-agnostic AI Remove, with a qualified local-model path and explicit owner decisions |
| [Dependency advisories](dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Product decisions](product-decisions.json) | Open product questions |

The Corrections plan is a planning proposal. Its AI implementation builds on the implemented [module capabilities](../docs/design/module-capabilities.md) and depends on owner acceptance of the scope and consequential product choices in the [Corrections design](../docs/design/corrections.md).

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
