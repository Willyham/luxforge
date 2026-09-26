# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

| Plan | Purpose |
| --- | --- |
| [Initial RAW editing](implementation-initial-raw.json) | Continuous high-precision RAW recipes for the original Nikon Z6, Fujifilm X100VI and supplied DJI Air 2S DNG: mode qualification, decoder/development evidence, bounded preparation, neutral rendering, exposure/WB, history and UI/API parity |
| [Corrections](corrections.json) | Proposed offline Clone/Heal and optional provider-agnostic AI Remove, with a qualified local-model path and explicit owner decisions |
| [Dependency advisories](dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Product decisions](product-decisions.json) | Open product questions |
| [Consolidation](consolidation.json) | One implementation per cross-cutting mechanism: the defects where copies diverged, the core front doors, the module contract, the desktop draft driver, one render pipeline and the smoke harness, per the [design](../docs/design/consolidation.md) |
| [Desktop design alignment](desktop-design-alignment.json) | The shell (title bar, state panel, canvas chrome, histogram, status bar) brought into line with the Develop workspace boards and the owner's 2026-09-26 decisions |
| [Known bugs](known-bugs.json) | Independent defects found in use, each with its measured symptom and acceptance |

The Corrections plan is a planning proposal. Its AI implementation builds on the implemented [module capabilities](../docs/design/module-capabilities.md) and depends on owner acceptance of the scope and consequential product choices in the [Corrections design](../docs/design/corrections.md).

RAW editing is implemented with initial native M4 verification. Its [design](../docs/design/initial-raw.md) records the owner's continuous RAW editing requirement, the pinned processing path, and outstanding controlled quality, resource and platform qualification. The supplied FC3411 DNG has required gain/warp corrections and continuous editor support; its [contract](../docs/design/air2s-dng.md) records the qualified encoding, numerical interpretation and limits. RAW JPEG-export integration requires the shared exporter on the [roadmap](../docs/plan.md) and does not block the RAW editing checkpoint.

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- A task's `test_strategy.commands` are its acceptance checks, run once when the task is complete, not after each step. Put timing commands in the task that completes the feature, or in a task whose subject is performance, so a plan measures once.
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links do not point into another task plan. The advisory audit reads the dependency advisories plan to confirm each exception's task remains open.
